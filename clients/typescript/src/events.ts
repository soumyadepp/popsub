/**
 * What the client reports about its connection and subscriptions, and the
 * listener registry that delivers it.
 *
 * @module
 */

import type { CloseCode, ErrorCode } from "./protocol.js";

/** A connection is up and authenticated. */
export interface ConnectedEvent {
  /** The event's name. */
  type: "connected";
  /** Topics the server restored for the durable session. */
  resumed: string[];
}

/** The connection went down. */
export interface DisconnectedEvent {
  /** The event's name. */
  type: "disconnected";
  /** The server's close code, or `null` when it sent none the client knows. */
  code: CloseCode | null;
  /** Whether the client will try to reconnect. */
  reconnecting: boolean;
}

/** Messages on a topic were trimmed before the client got them. */
export interface TruncatedEvent {
  /** The event's name. */
  type: "truncated";
  /** The topic. */
  topic: string;
  /** The oldest `seq` still available; delivery continues from here. */
  firstAvailable: number;
}

/**
 * A QoS 1 message ran out of retries. The server sends the topic nothing
 * more until the client acknowledges something or subscribes again.
 */
export interface StalledEvent {
  /** The event's name. */
  type: "stalled";
  /** The topic. */
  topic: string;
  /** The message that was never acknowledged. */
  seq: number;
}

/**
 * The token expires soon. With password credentials the client refreshes
 * it on its own; with a bare token the connection closes when it expires.
 */
export interface AuthExpiringEvent {
  /** The event's name. */
  type: "authExpiring";
  /** When it expires, in milliseconds since the Unix epoch. */
  expiresAtMs: number;
}

/** The client got a new token and the connection carries on. */
export interface AuthRefreshedEvent {
  /** The event's name. */
  type: "authRefreshed";
}

/**
 * The server ended a subscription because the user may no longer read the
 * topic. Its `Subscription` ends and the client won't resubscribe.
 */
export interface UnsubscribedEvent {
  /** The event's name. */
  type: "unsubscribed";
  /** The topic. */
  topic: string;
}

/** The server reported an error that answers no request, such as a failed QoS 0 publish. */
export interface ServerErrorEvent {
  /** The event's name. */
  type: "serverError";
  /** What went wrong. */
  code: ErrorCode;
  /** What went wrong, for people. */
  message: string;
}

/** Something that happened to the connection or a subscription. */
export type ClientEvent =
  | ConnectedEvent
  | DisconnectedEvent
  | TruncatedEvent
  | StalledEvent
  | AuthExpiringEvent
  | AuthRefreshedEvent
  | UnsubscribedEvent
  | ServerErrorEvent;

/** A function called with every event. */
export type ClientEventListener = (event: ClientEvent) => void;

/**
 * Calls every registered listener with each event. A listener that throws
 * doesn't stop the others or the client: its error is rethrown from a
 * microtask, where the runtime reports it as an uncaught error.
 */
export class Emitter {
  /** The registered listeners. */
  readonly #listeners = new Set<ClientEventListener>();

  /**
   * Registers a listener.
   *
   * @param listener - Called with every event from now on.
   * @returns A function that removes the listener.
   */
  on(listener: ClientEventListener): () => void {
    this.#listeners.add(listener);
    return () => {
      this.off(listener);
    };
  }

  /**
   * Removes a listener.
   *
   * @param listener - The listener passed to `on`.
   */
  off(listener: ClientEventListener): void {
    this.#listeners.delete(listener);
  }

  /** Removes every listener. */
  clear(): void {
    this.#listeners.clear();
  }

  /**
   * Calls every listener with an event.
   *
   * @param event - The event.
   */
  emit(event: ClientEvent): void {
    for (const listener of [...this.#listeners]) {
      try {
        listener(event);
      } catch (error) {
        queueMicrotask(() => {
          throw error;
        });
      }
    }
  }
}
