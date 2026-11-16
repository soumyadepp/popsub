/**
 * The engine behind a `Client`: it owns the WebSocket and does what every
 * application would otherwise have to.
 *
 * It connects, logs in and authenticates, then serves the connection until
 * it ends. The close code decides what happens next: reconnect with
 * jittered backoff, or stop for good (`session_taken_over`,
 * `unsupported_protocol`, `auth_failed`, `account_disabled`, rejected
 * credentials, and `token_expired` with token credentials). After
 * reconnecting it resubscribes every topic from the next `seq` it expects
 * (topics a durable session restored on its own are left to the server),
 * resends confirmed publishes that never got their `published` under the
 * same request id and `message_id`, and sends QoS 0 publishes and
 * unsubscribes queued while disconnected.
 *
 * Every message is checked against the next `seq` its topic expects, so a
 * retry or a replay never reaches the application twice. A duplicate the
 * application acknowledged before (or any duplicate in automatic mode,
 * unless its acknowledgement is held back for a full buffer) is
 * acknowledged again, so a lost acknowledgement can't stall the topic.
 *
 * An `unsubscribed` frame with no id means the server ended the
 * subscription; every unsubscribe the engine sends carries an id so its
 * reply is never mistaken for that.
 *
 * The browser `WebSocket` API can't pause reading, so backpressure works in
 * two steps. While a subscription's buffer holds `subscriptionBuffer`
 * messages or more, automatic acknowledgements of new QoS 1 messages wait
 * until the application takes them; the server's per-subscription window
 * (`limits.max_inflight`) then stops sending to it, QoS 0 included. If the
 * buffer still reaches `subscriptionBuffer + max_inflight` (possible with
 * QoS 0 traffic, which no window paces), the engine drops the connection,
 * waits until the application has taken that buffer down to half of
 * `subscriptionBuffer`, reconnects, and resubscribes that topic from the
 * next `seq` it expects, so the log fills the gap. Memory per subscription
 * stays below `subscriptionBuffer + max_inflight` messages.
 *
 * Nor does the API show the server's WebSocket pings, so silence is
 * detected with the protocol's own `ping` request: after `heartbeat_secs`
 * with nothing from the server the engine sends `ping` with an id, and
 * again after each further `heartbeat_secs` of silence. Any frame,
 * including the `pong` reply, counts as hearing from the server. Once
 * `idle_timeout_secs + heartbeat_secs` pass with nothing at all, the
 * connection is given up and the engine reconnects.
 *
 * @module
 */

import { jitteredDelay } from "./backoff.js";
import type { ResolvedConfig } from "./config.js";
import { DeliveryTracker } from "./dedup.js";
import { Emitter } from "./events.js";
import { PopSubError } from "./errors.js";
import { Aborted, ConnectFailure, Handshake } from "./handshake.js";
import {
  closeCode,
  encodeFrame,
  errorCode,
  parseServerFrame,
  PROTOCOL_VERSION,
  shouldReconnect,
  type ClientMessage,
  type CloseCode,
  type Limits,
  type ServerFrame,
  type ServerMessage,
  type WireStartFrom,
} from "./protocol.js";
import { Channel, Message, Subscription } from "./subscription.js";

/** How many QoS 0 publishes and unsubscribes are held while disconnected; the oldest go first. */
export const OFFLINE_LIMIT = 10_000;

/** How long to wait for anything from the server before the server has announced its limits. */
const DEFAULT_SILENCE_MS = 120_000;

/** How long a connection may be silent before a `ping`, before the server has announced its limits. */
const DEFAULT_PING_MS = 30_000;

/** The window size assumed before the server has announced its limits. */
const DEFAULT_MAX_INFLIGHT = 100;

/** The result of a confirmed publish. */
export interface Published {
  /** The message's position in the topic. */
  seq: number;
  /** The id it was stored under. */
  messageId: string;
}

/** How to settle a caller's promise. */
interface Settle<T> {
  /** Fulfils it. */
  resolve(value: T): void;
  /** Rejects it. */
  reject(error: PopSubError): void;
}

/** A request waiting for its reply, by request id. */
type Pending =
  | { kind: "subscribe"; topic: string; generation: number }
  | { kind: "unsubscribe"; settle: Settle<void> }
  | { kind: "publish"; settle: Settle<Published> }
  | { kind: "request"; settle: Settle<ServerMessage> }
  | { kind: "relogin" }
  | { kind: "reauth" }
  | { kind: "ping" };

/** One subscribed topic. */
interface Topic {
  /** Where its messages go. */
  channel: Channel;
  /** Where replay started when the application subscribed. */
  from: WireStartFrom;
  /** Which subscription this is, so a stale close can't remove a newer one. */
  generation: number;
  /** The next expected `seq` and acknowledgement memory. */
  tracker: DeliveryTracker;
  /** Whether its buffer overflowed, so deliveries are dropped until it is resubscribed. */
  overflowed: boolean;
}

/** How a served connection ended. */
type Ending = { kind: "stop" } | { kind: "lost"; code: CloseCode | null } | { kind: "pause" };

/** The outcome of one connection attempt. */
type Attempt =
  | { kind: "connected"; handshake: Handshake; resumed: string[] }
  | { kind: "failed"; failure: ConnectFailure }
  | { kind: "stopped" };

/**
 * Creates a version 4 UUID, with `crypto.randomUUID` where the runtime has
 * it (browsers only offer it on secure origins) and `getRandomValues`
 * otherwise.
 *
 * @returns The UUID.
 */
export function uuid(): string {
  const crypto = globalThis.crypto;
  if (typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/**
 * Turns an unexpected reply during connecting into a failure.
 *
 * @param reply - What the server sent instead of the expected reply.
 * @returns The failure; retryable only for the server's temporary refusals
 *   and for replies of the wrong type.
 */
function refusal(reply: ServerMessage): ConnectFailure {
  if (reply.type === "error") {
    const error = PopSubError.server(errorCode(reply.code), reply.message, reply.retry_after ?? null);
    return new ConnectFailure(error, error.isTemporary());
  }
  return new ConnectFailure(new PopSubError("protocol", `unexpected ${reply.type} while connecting`), true);
}

/** The state and the run loop behind one `Client`. */
export class Engine {
  /** How to connect. */
  readonly #config: ResolvedConfig;
  /** Where events go. */
  readonly emitter = new Emitter();
  /** Subscribed topics. */
  readonly #topics = new Map<string, Topic>();
  /** Subscribes waiting for `subscribed`, by topic. */
  readonly #subscribeWaiters = new Map<string, Settle<void>>();
  /** Requests waiting for a reply, by id. */
  readonly #pending = new Map<number, Pending>();
  /** Confirmed publishes not yet confirmed, by request id, in id order, for resending. */
  readonly #unconfirmed = new Map<number, ClientMessage>();
  /** QoS 0 publishes and unsubscribes to send once connected. */
  #offline: ClientMessage[] = [];
  /** The next request id. */
  #nextId = 1;
  /** The next subscription generation. */
  #nextGeneration = 1;
  /** Whether a connection has ever succeeded, so `clean_start` is only sent once. */
  #connectedOnce = false;
  /** How long to wait for anything from the server before giving up on the connection. */
  #silenceMs = DEFAULT_SILENCE_MS;
  /** How long a connection may be silent before a `ping`, and between `ping`s while it stays silent. */
  #pingMs = DEFAULT_PING_MS;
  /** The server's per-subscription window. */
  #maxInflight = DEFAULT_MAX_INFLIGHT;
  /** The served connection, while there is one. */
  #connection: Handshake | null = null;
  /** Ends the served connection, while there is one. */
  #endServe: ((ending: Ending) => void) | null = null;
  /** Re-checks whatever the run loop is waiting for; set by each wait. */
  #wake: (() => void) | null = null;
  /** Whether the application asked to close. */
  #closing = false;
  /** Why the client stopped, once it has. */
  #stopped: { code: CloseCode | null } | null = null;
  /** Settles when the first connection is up, or rejects with what stopped the client first. */
  readonly ready: Promise<void>;
  /** Settles `ready`. */
  #ready: Settle<void> | null = null;
  /** Settles when the run loop has finished and the client has stopped. */
  readonly finished: Promise<void>;
  /** Settles `finished`. */
  #finish: () => void = () => undefined;

  /**
   * Builds an engine and starts its run loop.
   *
   * @param config - How to connect.
   */
  constructor(config: ResolvedConfig) {
    this.#config = config;
    this.ready = new Promise<void>((resolve, reject) => {
      this.#ready = { resolve, reject };
    });
    this.ready.catch(() => undefined);
    this.finished = new Promise<void>((resolve) => {
      this.#finish = resolve;
    });
    void this.#run();
  }

  /**
   * Why the client stopped, if it has.
   *
   * @returns `{ code }` once stopped (the code is `null` when the
   *   application closed it or it ran out of attempts), `null` while running.
   */
  get stopped(): { code: CloseCode | null } | null {
    return this.#stopped;
  }

  /**
   * Subscribes to a topic, replacing any earlier subscription to it.
   *
   * @param topic - The topic.
   * @param from - Where replay starts.
   * @returns The subscription, and a promise that settles when the server
   *   confirms it.
   * @throws PopSubError of kind `closed` when the client has stopped.
   */
  subscribe(topic: string, from: WireStartFrom): { subscription: Subscription; done: Promise<void> } {
    this.#throwIfStopped();
    const generation = this.#nextGeneration++;
    const channel = new Channel(topic, {
      taken: (message) => {
        this.#taken(topic, generation, message);
      },
      leave: () => {
        this.#forget(topic, generation);
      },
    });
    const previous = this.#topics.get(topic);
    previous?.channel.end();
    this.#subscribeWaiters
      .get(topic)
      ?.reject(new PopSubError("protocol", `replaced by a later subscribe to '${topic}'`));
    this.#topics.set(topic, {
      channel,
      from,
      generation,
      tracker: new DeliveryTracker(),
      overflowed: false,
    });
    const done = new Promise<void>((resolve, reject) => {
      this.#subscribeWaiters.set(topic, { resolve, reject });
    });
    if (this.#connection !== null) {
      this.#sendSubscribe(topic, generation, from);
    }
    return { subscription: new Subscription(channel), done };
  }

  /**
   * Unsubscribes from a topic. While disconnected the unsubscribe is queued
   * and the call succeeds at once.
   *
   * @param topic - The topic.
   * @returns A promise that settles with the server's reply.
   */
  unsubscribe(topic: string): Promise<void> {
    if (this.#stopped !== null) {
      return Promise.reject(PopSubError.closed(this.#stopped.code));
    }
    this.#dropTopic(topic);
    if (this.#connection === null) {
      this.#queueOffline({ type: "unsubscribe", topic });
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      const id = this.#takeId();
      this.#pending.set(id, { kind: "unsubscribe", settle: { resolve, reject } });
      this.#send({ type: "unsubscribe", topic }, id);
    });
  }

  /**
   * Publishes at QoS 0. While disconnected the publish is queued.
   *
   * @param topic - The topic.
   * @param payload - The body.
   * @throws PopSubError of kind `closed` when the client has stopped.
   */
  publish(topic: string, payload: string): void {
    this.#throwIfStopped();
    this.#queueOffline({ type: "publish", topic, payload });
    this.#flushOffline();
  }

  /**
   * Publishes at QoS 1 under a new `message_id`, resending it after every
   * reconnect until the server confirms it.
   *
   * @param topic - The topic.
   * @param payload - The body.
   * @returns A promise that settles with the `seq` and id once stored.
   */
  publishConfirmed(topic: string, payload: string): Promise<Published> {
    if (this.#stopped !== null) {
      return Promise.reject(PopSubError.closed(this.#stopped.code));
    }
    return new Promise<Published>((resolve, reject) => {
      const id = this.#takeId();
      const message: ClientMessage = { type: "publish", topic, payload, message_id: uuid(), qos: 1 };
      this.#pending.set(id, { kind: "publish", settle: { resolve, reject } });
      this.#unconfirmed.set(id, message);
      if (this.#connection !== null) {
        this.#send(message, id);
      }
    });
  }

  /**
   * Acknowledges a message (from `Message.ack`). While disconnected it is
   * only remembered, so a redelivery is acknowledged again.
   *
   * @param topic - The topic.
   * @param seq - The message's `seq`.
   */
  ack(topic: string, seq: number): void {
    this.#topics.get(topic)?.tracker.acked.insert(seq);
    this.#send({ type: "ack", topic, seq });
  }

  /**
   * Sends a request that is never queued while disconnected and never
   * resent (the admin requests), and returns its reply.
   *
   * @param message - The request.
   * @returns A promise that settles with the reply, rejecting with a
   *   `server` error for an `error` frame, or `connect` when disconnected
   *   or when the connection drops first.
   */
  request(message: ClientMessage): Promise<ServerMessage> {
    if (this.#stopped !== null) {
      return Promise.reject(PopSubError.closed(this.#stopped.code));
    }
    if (this.#connection === null) {
      return Promise.reject(
        new PopSubError("connect", "not connected; this request isn't queued while disconnected"),
      );
    }
    return new Promise<ServerMessage>((resolve, reject) => {
      const id = this.#takeId();
      this.#pending.set(id, { kind: "request", settle: { resolve, reject } });
      this.#send(message, id);
    });
  }

  /**
   * Closes the connection and stops the client. Pending requests fail with
   * a `closed` error.
   *
   * @returns A promise that settles once the client has stopped.
   */
  close(): Promise<void> {
    if (this.#stopped === null) {
      this.#closing = true;
      this.#wake?.();
    }
    return this.finished;
  }

  /**
   * Runs until the client stops, then stops it. An unexpected exception
   * stops the client too, and becomes the cause of the error `ready`
   * rejects with, so the returned promise never rejects.
   */
  async #run(): Promise<void> {
    try {
      this.#stop(await this.#loop());
    } catch (error) {
      this.#stop(null, error);
    }
  }

  /**
   * Connects, serves the connection, reconnects with backoff, and settles
   * `ready` with the first outcome, until the client must stop.
   *
   * @returns The close code that stopped the client, or `null` when the
   *   application closed it, it ran out of attempts, or connecting failed
   *   in a way retrying can't fix.
   */
  async #loop(): Promise<CloseCode | null> {
    let attempt = 0;
    let lastError: PopSubError | null = null;
    while (!this.#closing) {
      const outcome = await this.#connectOnce();
      if (outcome.kind === "stopped") {
        break;
      }
      if (outcome.kind === "connected") {
        attempt = 0;
        this.#connectedOnce = true;
        this.#ready?.resolve();
        this.#ready = null;
        this.emitter.emit({ type: "connected", resumed: outcome.resumed });
        const ending = await this.#serve(outcome.handshake, outcome.resumed);
        if (ending.kind === "stop") {
          break;
        }
        this.#failRequests();
        if (ending.kind === "pause") {
          this.emitter.emit({ type: "disconnected", code: null, reconnecting: true });
          if (!(await this.#waitForRoom())) {
            break;
          }
          continue;
        }
        const reconnecting = this.#shouldReconnect(ending.code);
        this.emitter.emit({ type: "disconnected", code: ending.code, reconnecting });
        if (!reconnecting) {
          return ending.code;
        }
      } else {
        lastError = outcome.failure.error;
        if (!outcome.failure.retry) {
          this.#ready?.reject(outcome.failure.error);
          this.#ready = null;
          break;
        }
      }
      attempt += 1;
      const maxAttempts = this.#config.reconnect.maxAttempts;
      if (maxAttempts !== null && attempt > maxAttempts) {
        const reason = lastError === null ? "" : `; the last attempt failed: ${lastError.message}`;
        this.#ready?.reject(
          new PopSubError("connect", `ran out of reconnect attempts${reason}`, { cause: lastError }),
        );
        this.#ready = null;
        break;
      }
      if (!(await this.#sleep(jitteredDelay(this.#config.reconnect, attempt)))) {
        break;
      }
    }
    return null;
  }

  /**
   * Decides whether to reconnect after a connection was lost.
   *
   * @param code - The server's close code, if it sent one.
   * @returns True to reconnect. An expired token only allows it with
   *   password credentials, which log in again.
   */
  #shouldReconnect(code: CloseCode | null): boolean {
    if (code === null) {
      return true;
    }
    if (code === 4004) {
      return "password" in this.#config.credentials;
    }
    return shouldReconnect(code);
  }

  /**
   * Waits before the next attempt, unless the application closes the client.
   *
   * @param ms - How long to wait.
   * @returns False when the application closed the client while waiting.
   */
  #sleep(ms: number): Promise<boolean> {
    return new Promise((resolve) => {
      if (this.#closing) {
        resolve(false);
        return;
      }
      const timer = setTimeout(() => {
        this.#wake = null;
        resolve(true);
      }, ms);
      this.#wake = () => {
        if (this.#closing) {
          clearTimeout(timer);
          this.#wake = null;
          resolve(false);
        }
      };
    });
  }

  /**
   * After a buffer overflowed, waits until every overflowed topic's buffer
   * is down to half of `subscriptionBuffer` (or the topic is gone).
   *
   * @returns False when the application closed the client while waiting.
   */
  #waitForRoom(): Promise<boolean> {
    const low = Math.floor(this.#config.subscriptionBuffer / 2);
    const drained = (): boolean =>
      [...this.#topics.values()].every((topic) => !topic.overflowed || topic.channel.size <= low);
    return new Promise((resolve) => {
      const check = (): void => {
        if (this.#closing) {
          this.#wake = null;
          resolve(false);
        } else if (drained()) {
          this.#wake = null;
          resolve(true);
        }
      };
      this.#wake = check;
      check();
    });
  }

  /**
   * Opens a connection, logs in if needed, and authenticates.
   *
   * @returns The authenticated connection with the topics the server
   *   restored, a failure saying whether retrying could help, or `stopped`
   *   when the application closed the client meanwhile.
   */
  async #connectOnce(): Promise<Attempt> {
    let handshake: Handshake;
    try {
      handshake = new Handshake(this.#config.url, this.#config.requestTimeoutMs);
    } catch (error) {
      const failure =
        error instanceof PopSubError ? error : new PopSubError("config", String(error), { cause: error });
      return { kind: "failed", failure: new ConnectFailure(failure, false) };
    }
    this.#wake = () => {
      if (this.#closing) {
        handshake.abort();
      }
    };
    try {
      await handshake.opened();
      const credentials = this.#config.credentials;
      let token: string;
      if ("token" in credentials) {
        token = credentials.token;
      } else {
        const reply = await handshake.exchange(this.#takeId(), {
          type: "login",
          username: credentials.username,
          password: credentials.password,
        });
        if (reply.type !== "logged_in") {
          throw refusal(reply);
        }
        token = reply.token;
      }
      const auth: ClientMessage = { type: "auth", token, protocol: PROTOCOL_VERSION };
      if (this.#config.sessionId !== null) {
        auth.session_id = this.#config.sessionId;
      }
      if (this.#config.cleanStart && !this.#connectedOnce) {
        auth.clean_start = true;
      }
      const reply = await handshake.exchange(this.#takeId(), auth);
      if (reply.type !== "authenticated") {
        throw refusal(reply);
      }
      this.#applyLimits(reply.limits);
      return { kind: "connected", handshake, resumed: reply.resumed ?? [] };
    } catch (reason) {
      handshake.close();
      if (reason instanceof Aborted) {
        return { kind: "stopped" };
      }
      if (reason instanceof ConnectFailure) {
        return { kind: "failed", failure: reason };
      }
      const message = reason instanceof Error ? reason.message : String(reason);
      return {
        kind: "failed",
        failure: new ConnectFailure(new PopSubError("connect", message, { cause: reason }), true),
      };
    } finally {
      this.#wake = null;
    }
  }

  /**
   * Reads the server's limits: when to `ping`, how much silence to allow,
   * and the window size.
   *
   * @param limits - The limits from `authenticated`.
   */
  #applyLimits(limits: Limits): void {
    const idle = Math.max(1, limits.idle_timeout_secs);
    const heartbeat = Math.max(1, limits.heartbeat_secs);
    this.#pingMs = heartbeat * 1000;
    this.#silenceMs = (idle + heartbeat) * 1000;
    if (Number.isFinite(limits.max_inflight) && limits.max_inflight > 0) {
      this.#maxInflight = limits.max_inflight;
    }
  }

  /**
   * Serves one connection until it ends. Restores subscriptions and resends
   * first, then handles frames as they arrive. A timer sends `ping` while
   * the connection is silent and ends it as lost once the silence passes
   * `idle_timeout_secs + heartbeat_secs`.
   *
   * @param handshake - The authenticated connection.
   * @param resumed - Topics the server restored for the session.
   * @returns How the connection ended.
   */
  #serve(handshake: Handshake, resumed: string[]): Promise<Ending> {
    return new Promise<Ending>((resolve) => {
      let lastHeard = Date.now();
      let lastPing = lastHeard;
      const tick = Math.max(50, Math.min(1000, this.#pingMs / 4));
      const timer = setInterval(() => {
        const now = Date.now();
        if (now - lastHeard >= this.#silenceMs) {
          finish({ kind: "lost", code: null });
        } else if (now - lastHeard >= this.#pingMs && now - lastPing >= this.#pingMs) {
          lastPing = now;
          const id = this.#takeId();
          this.#pending.set(id, { kind: "ping" });
          this.#send({ type: "ping" }, id);
        }
      }, tick);
      const live = (): boolean => this.#connection === handshake;
      const finish = (ending: Ending): void => {
        if (!live()) {
          return;
        }
        clearInterval(timer);
        this.#connection = null;
        this.#endServe = null;
        this.#wake = null;
        handshake.close();
        resolve(ending);
      };
      this.#connection = handshake;
      this.#endServe = finish;
      this.#wake = () => {
        if (this.#closing) {
          finish({ kind: "stop" });
        }
      };
      const early = handshake.handOver({
        text: (text) => {
          lastHeard = Date.now();
          lastPing = lastHeard;
          this.#onText(text);
        },
        closed: (code) => {
          finish({ kind: "lost", code: closeCode(code) });
        },
      });
      this.#restore(resumed);
      for (const text of early) {
        if (!live()) {
          return;
        }
        this.#onText(text);
      }
    });
  }

  /**
   * Restores state on a new connection: resubscribes, resends unconfirmed
   * publishes, and sends queued frames.
   *
   * Topics the server restored for the session are left to it, except
   * those whose buffer overflowed; every other topic is subscribed again
   * from the next `seq` it expects, so nothing still in the log is missed.
   *
   * @param resumed - Topics the server restored.
   */
  #restore(resumed: string[]): void {
    const restored = new Set(resumed);
    for (const [name, topic] of this.#topics) {
      if (!restored.has(name) || topic.overflowed) {
        const next = topic.tracker.nextExpected;
        this.#sendSubscribe(name, topic.generation, next === null ? topic.from : { seq: next });
      }
      topic.overflowed = false;
    }
    for (const [id, message] of this.#unconfirmed) {
      this.#send(message, id);
    }
    this.#flushOffline();
  }

  /**
   * Sends a subscribe and records it as pending.
   *
   * @param topic - The topic.
   * @param generation - The subscription it is for.
   * @param from - Where replay starts.
   */
  #sendSubscribe(topic: string, generation: number, from: WireStartFrom): void {
    const id = this.#takeId();
    this.#pending.set(id, { kind: "subscribe", topic, generation });
    this.#send({ type: "subscribe", topic, from }, id);
  }

  /**
   * Queues a frame to send once connected, dropping the oldest beyond `OFFLINE_LIMIT`.
   *
   * @param message - A QoS 0 publish or an unsubscribe.
   */
  #queueOffline(message: ClientMessage): void {
    if (this.#offline.length >= OFFLINE_LIMIT) {
      this.#offline.shift();
    }
    this.#offline.push(message);
  }

  /** Sends every queued frame, if connected. Unsubscribes get an id. */
  #flushOffline(): void {
    if (this.#connection === null) {
      return;
    }
    const queued = this.#offline;
    this.#offline = [];
    for (const message of queued) {
      this.#send(message, message.type === "unsubscribe" ? this.#takeId() : undefined);
    }
  }

  /**
   * Sends a frame if connected. A frame sent while the socket is closing is
   * lost with it; the close that follows decides what is resent.
   *
   * @param message - The frame.
   * @param id - The request id, when a reply is expected.
   */
  #send(message: ClientMessage, id?: number): void {
    const socket = this.#connection?.socket;
    if (socket?.readyState !== WebSocket.OPEN) {
      return;
    }
    socket.send(encodeFrame(message, id));
  }

  /**
   * Takes a request id.
   *
   * @returns A new id.
   */
  #takeId(): number {
    const id = this.#nextId;
    this.#nextId += 1;
    return id;
  }

  /**
   * Removes a topic and ends its subscription.
   *
   * @param topic - The topic.
   * @returns Whether it was subscribed.
   */
  #dropTopic(topic: string): boolean {
    const entry = this.#topics.get(topic);
    if (entry === undefined) {
      return false;
    }
    this.#topics.delete(topic);
    entry.channel.end();
    return true;
  }

  /**
   * Unsubscribes without waiting, when the application closes a
   * subscription, if the topic still belongs to it.
   *
   * @param topic - The topic.
   * @param generation - The subscription being closed.
   */
  #forget(topic: string, generation: number): void {
    if (this.#topics.get(topic)?.generation !== generation) {
      return;
    }
    this.#dropTopic(topic);
    this.#subscribeWaiters.get(topic)?.reject(PopSubError.closed(null));
    this.#subscribeWaiters.delete(topic);
    if (this.#connection === null) {
      this.#queueOffline({ type: "unsubscribe", topic });
    } else {
      this.#send({ type: "unsubscribe", topic }, this.#takeId());
    }
    this.#wake?.();
  }

  /**
   * Called when the application takes a message: sends an acknowledgement
   * held back for a full buffer, and lets a paused client reconnect once
   * there is room.
   *
   * @param topic - The topic.
   * @param generation - The subscription the message came from.
   * @param message - The message taken.
   */
  #taken(topic: string, generation: number, message: Message): void {
    const entry = this.#topics.get(topic);
    if (entry?.generation === generation && entry.tracker.deferred.delete(message.seq)) {
      entry.tracker.acked.insert(message.seq);
      this.#send({ type: "ack", topic, seq: message.seq });
    }
    this.#wake?.();
  }

  /**
   * Parses a frame and acts on it; frames this client doesn't know are ignored.
   *
   * @param text - The frame's text.
   */
  #onText(text: string): void {
    const frame = parseServerFrame(text);
    if (frame !== null) {
      this.#onFrame(frame);
    }
  }

  /**
   * Acts on a frame from the server. A reply's pending entry is removed by
   * its id first, so a `pong` needs nothing more: hearing it at all is what
   * keeps the connection from being given up as silent.
   *
   * @param frame - The frame.
   */
  #onFrame(frame: ServerFrame): void {
    const id = typeof frame.id === "number" ? frame.id : null;
    const pending = id === null ? undefined : this.#pending.get(id);
    if (id !== null) {
      this.#pending.delete(id);
    }
    if (pending?.kind === "request") {
      if (frame.type === "error") {
        pending.settle.reject(
          PopSubError.server(errorCode(frame.code), frame.message, frame.retry_after ?? null),
        );
      } else {
        pending.settle.resolve(frame);
      }
      return;
    }
    const unprompted = frame.id === undefined || frame.id === null;
    switch (frame.type) {
      case "message":
        this.#deliver(frame);
        break;
      case "subscribed": {
        this.#topics.get(frame.topic)?.tracker.moveTo(frame.replay_from);
        const waiter = this.#subscribeWaiters.get(frame.topic);
        if (waiter !== undefined) {
          this.#subscribeWaiters.delete(frame.topic);
          waiter.resolve();
        }
        break;
      }
      case "unsubscribed":
        if (pending?.kind === "unsubscribe") {
          pending.settle.resolve();
        } else if (unprompted && this.#dropTopic(frame.topic)) {
          this.emitter.emit({ type: "unsubscribed", topic: frame.topic });
        }
        break;
      case "published":
        if (pending?.kind === "publish" && id !== null) {
          this.#unconfirmed.delete(id);
          pending.settle.resolve({ seq: frame.seq, messageId: frame.message_id });
        }
        break;
      case "truncated":
        this.#topics.get(frame.topic)?.tracker.moveTo(frame.first_available);
        this.emitter.emit({ type: "truncated", topic: frame.topic, firstAvailable: frame.first_available });
        break;
      case "stalled":
        this.emitter.emit({ type: "stalled", topic: frame.topic, seq: frame.seq });
        break;
      case "auth_expiring":
        this.emitter.emit({ type: "authExpiring", expiresAtMs: frame.expires_at_ms });
        if ("password" in this.#config.credentials) {
          const loginId = this.#takeId();
          this.#pending.set(loginId, { kind: "relogin" });
          const { username, password } = this.#config.credentials;
          this.#send({ type: "login", username, password }, loginId);
        }
        break;
      case "logged_in":
        if (pending?.kind === "relogin") {
          const authId = this.#takeId();
          this.#pending.set(authId, { kind: "reauth" });
          this.#send({ type: "auth", token: frame.token, protocol: PROTOCOL_VERSION }, authId);
        }
        break;
      case "authenticated":
        if (pending?.kind === "reauth") {
          this.emitter.emit({ type: "authRefreshed" });
        }
        break;
      case "pong":
        break;
      case "error":
        this.#onError(pending, id, frame);
        break;
      default:
        break;
    }
  }

  /**
   * Routes an error frame to the request it answers, or to the events.
   *
   * @param pending - The request it answers, if its id matched one.
   * @param id - Its request id, if it had one.
   * @param frame - The error frame.
   */
  #onError(
    pending: Pending | undefined,
    id: number | null,
    frame: Extract<ServerMessage, { type: "error" }>,
  ): void {
    const code = errorCode(frame.code);
    const error = PopSubError.server(code, frame.message, frame.retry_after ?? null);
    switch (pending?.kind) {
      case "subscribe": {
        if (this.#topics.get(pending.topic)?.generation === pending.generation) {
          this.#dropTopic(pending.topic);
          const waiter = this.#subscribeWaiters.get(pending.topic);
          this.#subscribeWaiters.delete(pending.topic);
          waiter?.reject(error);
        }
        break;
      }
      case "unsubscribe":
        pending.settle.reject(error);
        break;
      case "publish":
        if (id !== null) {
          this.#unconfirmed.delete(id);
        }
        pending.settle.reject(error);
        break;
      case "ping":
        break;
      default:
        this.emitter.emit({ type: "serverError", code, message: frame.message });
        break;
    }
  }

  /**
   * Hands a delivered message to its subscription, dropping duplicates,
   * acknowledging as the ack mode says, and holding back when the buffer is full.
   *
   * @param delivery - The delivered message.
   */
  #deliver(delivery: Extract<ServerMessage, { type: "message" }>): void {
    const { topic: name, seq } = delivery;
    const qos: 0 | 1 = delivery.qos === 1 ? 1 : 0;
    const topic = this.#topics.get(name);
    if (topic === undefined) {
      if (qos === 1) {
        this.#send({ type: "ack", topic: name, seq });
      }
      return;
    }
    if (topic.overflowed) {
      return;
    }
    const automatic = this.#config.ackMode === "auto";
    if (topic.tracker.isDuplicate(seq)) {
      if (qos === 1 && topic.tracker.reacksDuplicate(seq, automatic)) {
        this.#send({ type: "ack", topic: name, seq });
      }
      return;
    }
    const buffered = topic.channel.size;
    if (buffered >= this.#config.subscriptionBuffer + this.#maxInflight) {
      topic.overflowed = true;
      this.#endServe?.({ kind: "pause" });
      return;
    }
    topic.tracker.accept(seq);
    let acker: (() => void) | null = null;
    if (qos === 1) {
      if (!automatic) {
        acker = () => {
          this.ack(name, seq);
        };
      } else if (buffered < this.#config.subscriptionBuffer) {
        topic.tracker.acked.insert(seq);
        this.#send({ type: "ack", topic: name, seq });
      } else {
        topic.tracker.deferred.add(seq);
      }
    }
    topic.channel.push(
      new Message(
        {
          topic: name,
          seq,
          messageId: delivery.message_id,
          payload: delivery.payload,
          qos,
          publishedAtMs: delivery.published_at_ms,
        },
        acker,
      ),
    );
  }

  /**
   * Fails every admin request still waiting when a connection ends: the
   * server may or may not have carried it out. `ping`s, token refreshes and
   * subscribes of the old connection are forgotten; subscribes are sent
   * again by `#restore`.
   */
  #failRequests(): void {
    for (const [id, pending] of this.#pending) {
      if (pending.kind === "request") {
        this.#pending.delete(id);
        pending.settle.reject(
          new PopSubError(
            "connect",
            "the connection dropped before the reply; the request may or may not have been carried out",
          ),
        );
      } else if (
        pending.kind === "ping" ||
        pending.kind === "relogin" ||
        pending.kind === "reauth" ||
        pending.kind === "subscribe"
      ) {
        this.#pending.delete(id);
      }
    }
  }

  /**
   * Throws if the client has stopped.
   *
   * @throws PopSubError of kind `closed` with the close code that stopped it.
   */
  #throwIfStopped(): void {
    if (this.#stopped !== null) {
      throw PopSubError.closed(this.#stopped.code);
    }
  }

  /**
   * Stops the client for good: records why, fails everything waiting, ends
   * every subscription, and removes every event listener.
   *
   * @param code - The close code that stopped it, if any.
   * @param cause - The unexpected exception that stopped it, if any.
   */
  #stop(code: CloseCode | null, cause?: unknown): void {
    this.#endServe?.({ kind: "stop" });
    this.#wake = null;
    this.#stopped = { code };
    const error = PopSubError.closed(code);
    this.#ready?.reject(
      cause === undefined
        ? error
        : new PopSubError(
            "closed",
            `the client stopped unexpectedly: ${cause instanceof Error ? cause.message : "unknown error"}`,
            { cause },
          ),
    );
    this.#ready = null;
    for (const waiter of this.#subscribeWaiters.values()) {
      waiter.reject(error);
    }
    this.#subscribeWaiters.clear();
    for (const pending of this.#pending.values()) {
      if (pending.kind === "unsubscribe" || pending.kind === "publish" || pending.kind === "request") {
        pending.settle.reject(error);
      }
    }
    this.#pending.clear();
    this.#unconfirmed.clear();
    this.#offline = [];
    for (const topic of this.#topics.values()) {
      topic.channel.end();
    }
    this.#topics.clear();
    this.emitter.clear();
    this.#finish();
  }
}
