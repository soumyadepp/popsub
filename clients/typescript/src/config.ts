/**
 * How a `Client` connects: where, as whom, and how it behaves when things
 * go wrong. `resolveConfig` fills in defaults and rejects invalid values.
 *
 * @module
 */

import { PopSubError } from "./errors.js";

/**
 * A username and password. The client logs in on every connection and
 * refreshes its token on its own before it expires.
 */
export interface PasswordCredentials {
  /** The username. */
  username: string;
  /** The password. */
  password: string;
}

/**
 * A JWT obtained elsewhere. The client can't refresh it: when it expires
 * the server closes the connection with `token_expired` and the client
 * stops, after an `authExpiring` event.
 */
export interface TokenCredentials {
  /** The token. */
  token: string;
}

/** How the client proves who it is. */
export type Credentials = PasswordCredentials | TokenCredentials;

/**
 * When QoS 1 messages are acknowledged.
 *
 * - `auto`: as the client hands them to the subscription's buffer (or, when
 *   the buffer is full, as the application takes them). A message the
 *   application was processing when it crashed isn't redelivered.
 * - `manual`: when the application calls `Message.ack`. A message never
 *   acknowledged is redelivered after a reconnect (to a durable session),
 *   and stalls its subscription once the server runs out of retries.
 */
export type AckMode = "auto" | "manual";

/**
 * How the client waits between reconnection attempts: a random wait
 * between zero and a ceiling that doubles from `initialMs` up to `maxMs`.
 */
export interface ReconnectPolicy {
  /** The starting ceiling, in milliseconds. */
  initialMs: number;
  /** The highest ceiling, in milliseconds. */
  maxMs: number;
  /** Give up after this many failed attempts in a row; `null` keeps trying. */
  maxAttempts: number | null;
}

/** Everything a `Client` needs to connect. Only `url` and `credentials` are required. */
export interface ClientConfig {
  /** The server's WebSocket URL, such as `ws://127.0.0.1:8080`, or `wss://` for TLS. */
  url: string;
  /** Who to connect as. */
  credentials: Credentials;
  /**
   * A durable session to hold, 1 to 128 characters, so subscriptions and
   * unacknowledged messages survive disconnects. Absent or `null` is an
   * ephemeral connection.
   */
  sessionId?: string | null;
  /** Discard the session's stored subscriptions on the first connection. Never sent on reconnects. */
  cleanStart?: boolean;
  /** When QoS 1 messages are acknowledged; `auto` by default. */
  ackMode?: AckMode;
  /** How to reconnect; 250 ms doubling up to 30 seconds, forever, by default. */
  reconnect?: Partial<ReconnectPolicy>;
  /**
   * How long a request (connect, subscribe, unsubscribe, confirmed publish,
   * admin call) may take, reconnects included, in milliseconds; 30 seconds
   * by default.
   */
  requestTimeoutMs?: number;
  /**
   * How many messages each subscription buffers for the application before
   * the client starts holding back; 1024 by default. See the README's
   * section on backpressure for what happens beyond it.
   */
  subscriptionBuffer?: number;
}

/** A `ClientConfig` with every default filled in. */
export interface ResolvedConfig {
  /** The server's WebSocket URL. */
  url: string;
  /** Who to connect as. */
  credentials: Credentials;
  /** The durable session, or `null` for an ephemeral connection. */
  sessionId: string | null;
  /** Whether the first connection discards the session's stored subscriptions. */
  cleanStart: boolean;
  /** When QoS 1 messages are acknowledged. */
  ackMode: AckMode;
  /** How to reconnect. */
  reconnect: ReconnectPolicy;
  /** How long a request may take, in milliseconds. */
  requestTimeoutMs: number;
  /** How many messages each subscription buffers before holding back. */
  subscriptionBuffer: number;
}

/** The reconnect policy used for fields the configuration leaves out. */
export const DEFAULT_RECONNECT: Readonly<ReconnectPolicy> = Object.freeze({
  initialMs: 250,
  maxMs: 30_000,
  maxAttempts: null,
});

/** The request timeout used when the configuration leaves it out. */
export const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;

/** The subscription buffer size used when the configuration leaves it out. */
export const DEFAULT_SUBSCRIPTION_BUFFER = 1024;

/**
 * Builds the error for an invalid setting.
 *
 * @param message - What is wrong.
 * @returns The `config` error.
 */
function invalid(message: string): PopSubError {
  return new PopSubError("config", `invalid configuration: ${message}`);
}

/**
 * Checks that a setting is a positive, finite number.
 *
 * @param name - The setting's name, for the error.
 * @param value - Its value.
 * @returns The value.
 * @throws PopSubError of kind `config` when it isn't.
 */
function positive(name: string, value: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    throw invalid(`${name} must be a positive number`);
  }
  return value;
}

/**
 * Checks credentials and copies them, so later changes by the caller don't
 * reach the client.
 *
 * @param credentials - The credentials.
 * @returns The copy.
 * @throws PopSubError of kind `config` when they are neither a username and
 *   password nor a token.
 */
function checkCredentials(credentials: unknown): Credentials {
  if (typeof credentials !== "object" || credentials === null) {
    throw invalid("credentials are required");
  }
  const fields = credentials as Record<string, unknown>;
  const { token, username, password } = fields;
  if ("token" in fields) {
    if (typeof token !== "string" || token === "") {
      throw invalid("the token must be a non-empty string");
    }
    return { token };
  }
  if (typeof username !== "string" || typeof password !== "string") {
    throw invalid("credentials need a username and password, or a token");
  }
  return { username, password };
}

/**
 * Fills in defaults and checks every setting.
 *
 * @param config - The configuration from the application.
 * @returns The complete configuration.
 * @throws PopSubError of kind `config` for a URL that isn't `ws://` or
 *   `wss://`, credentials that are neither a password nor a token, a
 *   session id outside 1 to 128 characters, or a timeout, delay or buffer
 *   size that isn't positive.
 */
export function resolveConfig(config: ClientConfig): ResolvedConfig {
  const raw: unknown = config;
  if (typeof raw !== "object" || raw === null) {
    throw invalid("a configuration object is required");
  }
  if (typeof config.url !== "string" || !/^wss?:\/\//i.test(config.url)) {
    throw invalid("url must start with ws:// or wss://");
  }
  const sessionId = config.sessionId ?? null;
  if (
    sessionId !== null &&
    (typeof sessionId !== "string" || sessionId.length < 1 || sessionId.length > 128)
  ) {
    throw invalid("sessionId must be 1 to 128 characters");
  }
  const ackMode: unknown = config.ackMode ?? "auto";
  if (ackMode !== "auto" && ackMode !== "manual") {
    throw invalid("ackMode must be auto or manual");
  }
  const reconnect = { ...DEFAULT_RECONNECT, ...config.reconnect };
  positive("reconnect.initialMs", reconnect.initialMs);
  positive("reconnect.maxMs", reconnect.maxMs);
  if (
    reconnect.maxAttempts !== null &&
    (!Number.isInteger(reconnect.maxAttempts) || reconnect.maxAttempts < 0)
  ) {
    throw invalid("reconnect.maxAttempts must be a whole number of at least 0, or null");
  }
  const subscriptionBuffer = config.subscriptionBuffer ?? DEFAULT_SUBSCRIPTION_BUFFER;
  if (!Number.isInteger(subscriptionBuffer) || subscriptionBuffer < 1) {
    throw invalid("subscriptionBuffer must be a whole number of at least 1");
  }
  return {
    url: config.url,
    credentials: checkCredentials(config.credentials),
    sessionId,
    cleanStart: config.cleanStart ?? false,
    ackMode,
    reconnect,
    requestTimeoutMs: positive("requestTimeoutMs", config.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS),
    subscriptionBuffer,
  };
}
