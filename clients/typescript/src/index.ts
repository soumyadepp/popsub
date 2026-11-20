/**
 * The PopSub client for browsers and Node.js 22 or later.
 *
 * ```ts
 * import { Client } from "@popsub/client";
 *
 * const client = await Client.connect({
 *   url: "ws://127.0.0.1:8080",
 *   credentials: { username: "alice", password: "secret-password" },
 *   sessionId: "alice-laptop",
 * });
 * const orders = await client.subscribe("orders", "resume");
 * const stored = await client.publishConfirmed("orders", "order 42 shipped");
 * client.publishBytes("orders", new Uint8Array([0x00, 0xff]));
 * for await (const message of orders) {
 *   console.log(message.topic, message.seq, message.payload);
 * }
 * ```
 *
 * A payload is a string. `publishBytes` and `publishBytesConfirmed` send a
 * body that isn't text as base64, and `Message.bytes` returns the bytes of
 * any message.
 *
 * The protocol is documented in the repository's `docs/DESIGN.md` section 4.
 *
 * @module
 */

export { Admin, type TopicInfo, type User } from "./admin.js";
export { Client, type StartFrom } from "./client.js";
export type {
  AckMode,
  ClientConfig,
  Credentials,
  PasswordCredentials,
  ReconnectPolicy,
  TokenCredentials,
} from "./config.js";
export type { Published } from "./engine.js";
export { PopSubError, type ErrorKind } from "./errors.js";
export type {
  AuthExpiringEvent,
  AuthRefreshedEvent,
  ClientEvent,
  ClientEventListener,
  ConnectedEvent,
  DisconnectedEvent,
  ServerErrorEvent,
  StalledEvent,
  TruncatedEvent,
  UnsubscribedEvent,
} from "./events.js";
export {
  CloseCode,
  ERROR_CODES,
  PROTOCOL_VERSION,
  type Access,
  type Encoding,
  type ErrorCode,
  type RoleKind,
  type RoleSpec,
  type TopicGrant,
} from "./protocol.js";
export { Message, Subscription } from "./subscription.js";
