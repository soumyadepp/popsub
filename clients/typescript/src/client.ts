/**
 * `Client`, the handle the application holds.
 *
 * @module
 */

import { Admin } from "./admin.js";
import { resolveConfig, type ClientConfig, type ResolvedConfig } from "./config.js";
import { Engine, type Published } from "./engine.js";
import { PopSubError } from "./errors.js";
import type { ClientEventListener } from "./events.js";
import type { CloseCode, WireStartFrom } from "./protocol.js";
import type { Subscription } from "./subscription.js";

/**
 * Where replay starts when subscribing.
 *
 * - `"resume"`: one past a durable session's stored cursor for the topic;
 *   for an ephemeral connection, or a topic the session has no cursor for,
 *   the same as `"new"`.
 * - `"new"`: only messages published from now on.
 * - `{ last: n }`: the last `n` retained messages.
 * - `{ sinceMs: t }`: retained messages published at or after `t`
 *   (milliseconds since the Unix epoch).
 * - `{ seq: s }`: retained messages from `seq` `s` on.
 */
export type StartFrom = "resume" | "new" | { last: number } | { sinceMs: number } | { seq: number };

/**
 * Converts a start position to its wire form.
 *
 * @param from - The start position.
 * @returns The wire form.
 * @throws PopSubError of kind `config` when it is none of the allowed shapes.
 */
export function startFromToWire(from: StartFrom): WireStartFrom {
  if (from === "resume" || from === "new") {
    return from;
  }
  if (typeof from === "object") {
    if ("last" in from && Number.isInteger(from.last) && from.last >= 0) {
      return { last: from.last };
    }
    if ("sinceMs" in from && Number.isInteger(from.sinceMs)) {
      return { since_ms: from.sinceMs };
    }
    if ("seq" in from && Number.isInteger(from.seq) && from.seq >= 0) {
      return { seq: from.seq };
    }
  }
  throw new PopSubError("config", "from must be resume, new, { last }, { sinceMs } or { seq }");
}

/**
 * Waits for a promise for at most a given time.
 *
 * @param promise - What to wait for.
 * @param ms - The limit in milliseconds.
 * @returns The promise's value.
 * @throws PopSubError of kind `timeout` when the limit passes first, or
 *   whatever the promise rejects with.
 */
export function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(PopSubError.timeout());
    }, ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error instanceof Error ? error : new Error(String(error)));
      },
    );
  });
}

/**
 * A connection to a PopSub server that reconnects on its own, resubscribes
 * without gaps, resends confirmed publishes without duplicating them,
 * acknowledges QoS 1 messages and hands each `(topic, seq)` to the
 * application once.
 *
 * Create one with `Client.connect`. It runs until `close` is called or the
 * server stops it in a way that rules out reconnecting.
 */
export class Client {
  /** The engine that owns the connection. */
  readonly #engine: Engine;
  /** The configuration, for the request timeout. */
  readonly #config: ResolvedConfig;

  /**
   * Wraps a running engine. Use `Client.connect` instead.
   *
   * @param engine - The engine.
   * @param config - The configuration.
   */
  private constructor(engine: Engine, config: ResolvedConfig) {
    this.#engine = engine;
    this.#config = config;
  }

  /**
   * Connects to a server and starts the client.
   *
   * Waits for the first connection to log in and authenticate, retrying
   * with the reconnect policy, for up to `requestTimeoutMs`.
   *
   * @param config - Where and how to connect.
   * @param listener - Called with every event, starting with the first
   *   `connected`; more can be added with `on`.
   * @returns The connected client.
   * @throws PopSubError of kind `config` for an invalid configuration;
   *   `server` when the server refused the credentials (`invalid_credentials`),
   *   the protocol version or the session id; `closed` when the server
   *   closed the connection in a way that rules out retrying; `timeout`
   *   when no connection succeeded in time; `connect` when the reconnect
   *   policy ran out of attempts.
   */
  static async connect(config: ClientConfig, listener?: ClientEventListener): Promise<Client> {
    const resolved = resolveConfig(config);
    const engine = new Engine(resolved);
    if (listener !== undefined) {
      engine.emitter.on(listener);
    }
    try {
      await withTimeout(engine.ready, resolved.requestTimeoutMs);
    } catch (error) {
      await engine.close();
      throw error;
    }
    return new Client(engine, resolved);
  }

  /**
   * Subscribes to a topic. Subscribing again to the same topic replaces
   * the earlier subscription, which ends.
   *
   * @param topic - The topic.
   * @param from - Where replay starts; `"resume"` by default.
   * @returns The subscription, which yields messages in `seq` order.
   * @throws PopSubError of kind `server` when the server refused it (for
   *   example `not_authorized` or `invalid_topic`), `timeout` when it
   *   didn't complete in time, `closed` when the client has stopped, or
   *   `config` for an invalid `from`.
   */
  async subscribe(topic: string, from: StartFrom = "resume"): Promise<Subscription> {
    const { subscription, done } = this.#engine.subscribe(topic, startFromToWire(from));
    try {
      await withTimeout(done, this.#config.requestTimeoutMs);
    } catch (error) {
      subscription.close();
      throw error;
    }
    return subscription;
  }

  /**
   * Unsubscribes from a topic. Its subscription ends, and a durable session
   * forgets the stored subscription too. While disconnected the unsubscribe
   * is queued and the call succeeds at once.
   *
   * @param topic - The topic.
   * @throws PopSubError of kind `server` when the session store couldn't
   *   be updated, `timeout`, or `closed`.
   */
  async unsubscribe(topic: string): Promise<void> {
    await withTimeout(this.#engine.unsubscribe(topic), this.#config.requestTimeoutMs);
  }

  /**
   * Publishes a message at QoS 0: sent once, with no confirmation.
   *
   * While disconnected, up to 10,000 such publishes (and queued
   * unsubscribes) are held and sent after reconnecting; beyond that the
   * oldest are dropped. Failures the server reports arrive as
   * `serverError` events.
   *
   * @param topic - The topic.
   * @param payload - The body.
   * @throws PopSubError of kind `closed` when the client has stopped.
   */
  publish(topic: string, payload: string): void {
    this.#engine.publish(topic, payload);
  }

  /**
   * Publishes a message at QoS 1 and waits until the server has stored it.
   *
   * The message gets a new `message_id` (a UUID) and is resent under it,
   * with the same request id, after every reconnect until it is confirmed,
   * so the server stores it once.
   *
   * @param topic - The topic.
   * @param payload - The body.
   * @returns Its `seq` and `messageId`.
   * @throws PopSubError of kind `server` when the server refused it (for
   *   example `not_authorized`, `rate_limited` or `payload_too_large`),
   *   `timeout` when it wasn't confirmed in time (it may still be stored,
   *   and is still resent until confirmed), or `closed`.
   */
  publishConfirmed(topic: string, payload: string): Promise<Published> {
    return withTimeout(this.#engine.publishConfirmed(topic, payload), this.#config.requestTimeoutMs);
  }

  /**
   * The admin requests: creating accounts, granting and revoking topics,
   * disabling and deleting accounts. They need the client to be logged in
   * as an admin, and are never queued or resent.
   *
   * @returns A handle that sends them through this client.
   */
  admin(): Admin {
    return new Admin((request) => withTimeout(this.#engine.request(request), this.#config.requestTimeoutMs));
  }

  /**
   * Registers a listener for the client's events: connects, disconnects,
   * truncations, stalls, token refreshes, subscriptions the server ended,
   * and server errors that answer no request.
   *
   * @param listener - Called with every event from now on.
   * @returns A function that removes the listener.
   */
  on(listener: ClientEventListener): () => void {
    return this.#engine.emitter.on(listener);
  }

  /**
   * Removes a listener registered with `on` or `Client.connect`.
   *
   * @param listener - The listener.
   */
  off(listener: ClientEventListener): void {
    this.#engine.emitter.off(listener);
  }

  /**
   * Closes the connection and stops the client. Pending requests fail with
   * a `closed` error, every subscription ends, and no timer or socket is
   * left running.
   *
   * @returns A promise that settles once the client has stopped.
   */
  close(): Promise<void> {
    return this.#engine.close();
  }

  /**
   * Why the client stopped, if it has.
   *
   * @returns `{ code }` once stopped, where `code` is the close code that
   *   stopped it or `null` when the application closed it or it ran out of
   *   attempts; `null` while running.
   */
  get stopped(): { code: CloseCode | null } | null {
    return this.#engine.stopped;
  }
}
