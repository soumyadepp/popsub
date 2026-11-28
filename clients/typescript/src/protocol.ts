/**
 * The PopSub wire protocol, version 1: the JSON frames a client sends and
 * receives, the error codes and the WebSocket close codes, as described in
 * `docs/DESIGN.md` section 4 and `docs/protocol/*.json`.
 *
 * These types keep the protocol's own snake_case field names. The public
 * API converts them to camelCase at its edges.
 *
 * @module
 */

/** The protocol version this client speaks, sent in every `auth`. */
export const PROTOCOL_VERSION = 1;

/** A request id. This client only ever sends numbers. */
export type RequestId = number | string;

/** Where replay starts when subscribing, as it is written on the wire. */
export type WireStartFrom = "resume" | "new" | { last: number } | { since_ms: number } | { seq: number };

/**
 * How a payload string encodes a message's bytes when it isn't text.
 * `base64` is standard padded base64; a frame without an encoding carries text.
 */
export type Encoding = "base64";

/** What a topic grant allows. */
export type Access = "subscribe" | "publish" | "both";

/** The kind of account. */
export type RoleKind = "admin" | "user" | "readonly";

/** One topic pattern an account may use, and how. */
export interface TopicGrant {
  /** The topic pattern, such as `orders/#`. */
  pattern: string;
  /** What the pattern allows. */
  access: Access;
}

/** What an account may do. `topics` is ignored for admins. */
export interface RoleSpec {
  /** The kind of account. */
  kind: RoleKind;
  /** The account's topic grants. */
  topics?: TopicGrant[];
}

/** An account as the server describes it in `user` and `users` frames. */
export interface WireUser {
  /** The username. */
  username: string;
  /** What it may do. */
  role: RoleSpec;
  /** Whether it may log in and use topics. */
  enabled: boolean;
  /** When it was created, in milliseconds since the Unix epoch. */
  created_at_ms: number;
}

/** A topic as the server describes it in `topic_info` and `topics` frames. */
export interface WireTopicInfo {
  /** The topic's name. */
  name: string;
  /** The oldest `seq` still kept, or `last_seq + 1` when nothing is kept. */
  first_seq: number;
  /** The newest `seq` ever published. */
  last_seq: number;
  /** How many records are kept now. */
  messages: number;
  /** How many live subscriptions it has. */
  subscribers: number;
}

/** A backup as the server describes it in a `backup_created` frame. */
export interface WireBackupInfo {
  /** The backup's directory name. */
  name: string;
  /** Where the backup is on the server's file system. */
  path: string;
  /** How many topics it holds. */
  topics: number;
  /** How many records it holds across every topic. */
  records: number;
  /** How many bytes were written. */
  bytes: number;
  /** Whether accounts were included. */
  users: boolean;
}

/** The admin requests, which are never queued or resent. */
export type AdminRequest =
  | { type: "create_user"; username: string; password: string; role: RoleSpec }
  | { type: "get_user"; username: string }
  | { type: "list_users" }
  | { type: "set_role"; username: string; role: RoleSpec }
  | { type: "grant"; username: string; pattern: string; access: Access }
  | { type: "revoke"; username: string; pattern: string }
  | { type: "set_password"; username: string; password: string }
  | { type: "set_enabled"; username: string; enabled: boolean }
  | { type: "delete_user"; username: string }
  | { type: "list_topics" }
  | { type: "get_topic"; topic: string }
  | { type: "purge_topic"; topic: string }
  | { type: "backup"; name?: string };

/** Every frame this client sends, without its request id. */
export type ClientMessage =
  | { type: "login"; username: string; password: string }
  | {
      type: "auth";
      token: string;
      protocol?: number;
      session_id?: string;
      clean_start?: boolean;
      hold_resumed?: boolean;
    }
  | { type: "subscribe"; topic: string; from?: WireStartFrom; group?: string }
  | { type: "unsubscribe"; topic: string }
  | {
      type: "publish";
      topic: string;
      payload: string;
      encoding?: Encoding;
      message_id?: string;
      qos?: 0 | 1;
    }
  | { type: "ack"; topic: string; seq: number }
  | { type: "ping" }
  | AdminRequest;

/** The limits a server announces in `authenticated`. */
export interface Limits {
  /** How many QoS 1 records each subscription may have unacknowledged. */
  max_inflight: number;
  /** How long an untouched durable session is kept, in seconds. */
  session_expiry_secs: number;
  /** How often the server pings, in seconds. */
  heartbeat_secs: number;
  /** How long the server waits for anything from the client, in seconds. */
  idle_timeout_secs: number;
  /** The largest payload the server accepts, in bytes. */
  max_payload_bytes: number;
  /** The longest topic name, in bytes. */
  max_topic_bytes: number;
}

/** Every frame the server sends that this client reads, without its id. */
export type ServerMessage =
  | { type: "registered" }
  | { type: "logged_in"; token: string }
  | {
      type: "authenticated";
      protocol: number;
      limits: Limits;
      session_id?: string | null;
      resumed?: string[];
      held?: boolean;
    }
  | { type: "subscribed"; topic: string; replay_from: number; replay_to: number }
  | { type: "unsubscribed"; topic: string }
  | { type: "published"; message_id: string; seq: number }
  | {
      type: "message";
      topic: string;
      seq: number;
      message_id: string;
      payload: string;
      encoding?: Encoding | null;
      qos: number;
      published_at_ms: number;
    }
  | { type: "truncated"; topic: string; first_available: number }
  | { type: "stalled"; topic: string; seq: number }
  | { type: "auth_expiring"; expires_at_ms: number }
  | { type: "user"; user: WireUser }
  | { type: "users"; users: WireUser[] }
  | { type: "user_deleted"; username: string }
  | { type: "pong" }
  | { type: "topic_info"; topic: WireTopicInfo }
  | { type: "topics"; topics: WireTopicInfo[] }
  | { type: "backup_created"; backup: WireBackupInfo }
  | { type: "error"; code: string; message: string; retry_after?: number | null };

/** A server frame: a message plus the id of the request it answers, if any. */
export type ServerFrame = ServerMessage & { id?: RequestId | null };

/** Every error code protocol version 1 defines. */
export const ERROR_CODES = [
  "bad_request",
  "unauthenticated",
  "auth_failed",
  "invalid_credentials",
  "locked_out",
  "rate_limited",
  "not_authorized",
  "registration_failed",
  "invalid_session_id",
  "session_unavailable",
  "unsupported_protocol",
  "storage_unavailable",
  "payload_too_large",
  "invalid_topic",
  "forbidden",
  "user_not_found",
  "user_exists",
  "topic_not_found",
  "backup_exists",
  "backup_unsupported",
  "topic_limit_reached",
  "internal",
] as const;

/** Why the server refused a request. */
export type ErrorCode = (typeof ERROR_CODES)[number];

/** The codes `errorCode` recognises, for fast lookup. */
const KNOWN_ERROR_CODES: ReadonlySet<string> = new Set(ERROR_CODES);

/**
 * Reads an error code from a frame. The protocol says to treat a code this
 * client doesn't know like `internal`.
 *
 * @param raw - The `code` field as received.
 * @returns The code, or `internal` when it is unknown.
 */
export function errorCode(raw: unknown): ErrorCode {
  return typeof raw === "string" && KNOWN_ERROR_CODES.has(raw) ? (raw as ErrorCode) : "internal";
}

/** The WebSocket close codes a PopSub server sends, by name. */
export const CloseCode = {
  GoingAway: 1001,
  MessageTooBig: 1009,
  InternalError: 1011,
  TryAgainLater: 1013,
  AuthFailed: 4001,
  Unauthenticated: 4002,
  SessionTakenOver: 4003,
  TokenExpired: 4004,
  IdleTimeout: 4005,
  UnsupportedProtocol: 4006,
  SessionUnavailable: 4007,
  SlowConsumer: 4008,
  AccountDisabled: 4009,
} as const;

/** One of the close codes a PopSub server sends. */
export type CloseCode = (typeof CloseCode)[keyof typeof CloseCode];

/** The close codes `closeCode` recognises. */
const KNOWN_CLOSE_CODES: ReadonlySet<number> = new Set(Object.values(CloseCode));

/** The close codes after which the client never reconnects on its own. */
const FINAL_CLOSE_CODES: ReadonlySet<number> = new Set([
  CloseCode.AuthFailed,
  CloseCode.SessionTakenOver,
  CloseCode.UnsupportedProtocol,
  CloseCode.AccountDisabled,
]);

/**
 * Reads a close code from a WebSocket close event.
 *
 * @param raw - The close event's code.
 * @returns The PopSub close code, or `null` for any other code, such as
 *   1006 when the connection dropped without a close frame.
 */
export function closeCode(raw: number): CloseCode | null {
  return KNOWN_CLOSE_CODES.has(raw) ? (raw as CloseCode) : null;
}

/**
 * Whether a close code allows reconnecting, by the table in
 * `docs/protocol/close-codes.json`. The client adds one rule on top: after
 * `TokenExpired` it reconnects only with password credentials.
 *
 * @param code - The close code.
 * @returns False for `AuthFailed`, `SessionTakenOver`,
 *   `UnsupportedProtocol` and `AccountDisabled`, true otherwise.
 */
export function shouldReconnect(code: CloseCode): boolean {
  return !FINAL_CLOSE_CODES.has(code);
}

/** The fields each known server frame type must have, with their types. */
const REQUIRED_FIELDS: Readonly<Record<ServerMessage["type"], Readonly<Record<string, string>>>> = {
  registered: {},
  logged_in: { token: "string" },
  authenticated: { limits: "object" },
  subscribed: { topic: "string", replay_from: "number" },
  unsubscribed: { topic: "string" },
  published: { message_id: "string", seq: "number" },
  message: { topic: "string", seq: "number", message_id: "string", payload: "string", qos: "number" },
  truncated: { topic: "string", first_available: "number" },
  stalled: { topic: "string", seq: "number" },
  auth_expiring: { expires_at_ms: "number" },
  user: { user: "object" },
  users: { users: "object" },
  user_deleted: { username: "string" },
  pong: {},
  topic_info: { topic: "object" },
  topics: { topics: "object" },
  backup_created: { backup: "object" },
  error: { code: "string", message: "string" },
};

/**
 * Parses a text frame from the server.
 *
 * Frames that aren't JSON objects, have a `type` this client doesn't know,
 * or miss a field the client reads are ignored, as the protocol asks of
 * clients for frame types added later.
 *
 * @param text - The frame's text.
 * @returns The frame, or `null` when it should be ignored.
 */
export function parseServerFrame(text: string): ServerFrame | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  const record = value as Record<string, unknown>;
  const type = record.type;
  if (typeof type !== "string" || !Object.hasOwn(REQUIRED_FIELDS, type)) {
    return null;
  }
  const required = REQUIRED_FIELDS[type as ServerMessage["type"]];
  for (const [field, kind] of Object.entries(required)) {
    const present = record[field];
    if (typeof present !== kind || present === null) {
      return null;
    }
  }
  return record as unknown as ServerFrame;
}

/**
 * Builds a QoS 0 `publish` frame. A text publish carries no `encoding`
 * field at all, which is what a server reads as text.
 *
 * @param topic - The topic.
 * @param payload - The body: text, or base64 when `encoding` says so.
 * @param encoding - How `payload` encodes the body, or `null` for text.
 * @returns The frame.
 */
export function publishFrame(
  topic: string,
  payload: string,
  encoding: Encoding | null,
): Extract<ClientMessage, { type: "publish" }> {
  return encoding === null
    ? { type: "publish", topic, payload }
    : { type: "publish", topic, payload, encoding };
}

/**
 * Writes a frame as JSON text.
 *
 * @param message - The frame.
 * @param id - The request id, when a reply is expected.
 * @returns The JSON text.
 */
export function encodeFrame(message: ClientMessage, id?: RequestId): string {
  return JSON.stringify(id === undefined ? message : { ...message, id });
}
