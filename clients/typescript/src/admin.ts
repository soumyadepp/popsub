/**
 * `Admin`, the account management requests, for clients logged in as an
 * admin.
 *
 * Each call is one request and its reply. None is queued while the client
 * is disconnected or resent after a reconnect, because repeating a change
 * such as `createUser` isn't safe: a call made while disconnected fails
 * with a `connect` error, and so does one whose connection drops before the
 * reply, whether or not the server carried it out.
 *
 * @module
 */

import { PopSubError } from "./errors.js";
import type { Access, AdminRequest, RoleSpec, ServerMessage, WireUser } from "./protocol.js";

/** An account, as admins see it. Never includes the password. */
export interface User {
  /** The username. */
  username: string;
  /** What it may do. */
  role: RoleSpec;
  /** Whether it may log in and use topics. */
  enabled: boolean;
  /** When it was created, in milliseconds since the Unix epoch. */
  createdAtMs: number;
}

/** Sends one admin request and returns its reply. */
export type AdminTransport = (request: AdminRequest) => Promise<ServerMessage>;

/**
 * Converts an account from its wire form.
 *
 * @param user - The account as the server sent it.
 * @returns The account.
 */
function fromWire(user: WireUser): User {
  return {
    username: user.username,
    role: user.role,
    enabled: user.enabled,
    createdAtMs: user.created_at_ms,
  };
}

/**
 * Builds the error for a reply of the wrong type.
 *
 * @param reply - What the server sent.
 * @param expected - The type that was expected.
 * @returns The `protocol` error.
 */
function unexpected(reply: ServerMessage, expected: string): PopSubError {
  return new PopSubError("protocol", `expected a ${expected} reply, got ${reply.type}`);
}

/**
 * The admin requests, from `Client.admin`. Every call rejects with a
 * `server` error with code `forbidden` unless the client is logged in as
 * an enabled admin, `connect` when disconnected or when the connection
 * drops before the reply, `timeout` when no reply arrives in time, and
 * `closed` when the client has stopped.
 */
export class Admin {
  /** Sends the requests. */
  readonly #send: AdminTransport;

  /**
   * Wraps a transport. Applications get an `Admin` from `Client.admin`.
   *
   * @param send - Sends one request and returns its reply.
   */
  constructor(send: AdminTransport) {
    this.#send = send;
  }

  /**
   * Creates an account.
   *
   * @param username - Its name: 3 to 32 letters, digits, `_`, `.` or `-`.
   * @param password - Its password, at least 8 characters.
   * @param role - What it may do.
   * @returns The new account.
   * @throws PopSubError with code `user_exists`, or `bad_request` for an
   *   invalid name, password or pattern.
   */
  createUser(username: string, password: string, role: RoleSpec): Promise<User> {
    return this.#user({ type: "create_user", username, password, role });
  }

  /**
   * Reads an account.
   *
   * @param username - The account.
   * @returns The account.
   * @throws PopSubError with code `user_not_found`.
   */
  getUser(username: string): Promise<User> {
    return this.#user({ type: "get_user", username });
  }

  /**
   * Lists every account, sorted by name.
   *
   * @returns The accounts.
   * @throws PopSubError of kind `protocol` when the server answers with
   *   something other than `users`.
   */
  async listUsers(): Promise<User[]> {
    const reply = await this.#send({ type: "list_users" });
    if (reply.type !== "users") {
      throw unexpected(reply, "users");
    }
    return reply.users.map(fromWire);
  }

  /**
   * Replaces an account's role. Live subscriptions the new role doesn't
   * allow are ended.
   *
   * @param username - The account.
   * @param role - Its new role.
   * @returns The account as it now stands.
   * @throws PopSubError with code `user_not_found`, or `bad_request` for an
   *   invalid pattern or for demoting the last enabled admin.
   */
  setRole(username: string, role: RoleSpec): Promise<User> {
    return this.#user({ type: "set_role", username, role });
  }

  /**
   * Grants an account a topic pattern, replacing any grant it already has
   * for exactly that pattern.
   *
   * @param username - The account.
   * @param pattern - The topic pattern.
   * @param access - What it allows.
   * @returns The account as it now stands.
   * @throws PopSubError with code `user_not_found`, or `bad_request` for an
   *   invalid pattern, an admin account, or publishing for a readonly one.
   */
  grant(username: string, pattern: string, access: Access): Promise<User> {
    return this.#user({ type: "grant", username, pattern, access });
  }

  /**
   * Removes one of an account's grants. Live subscriptions no longer
   * allowed are ended.
   *
   * @param username - The account.
   * @param pattern - The pattern, exactly as granted.
   * @returns The account as it now stands.
   * @throws PopSubError with code `user_not_found`, or `bad_request` when
   *   the account has no such grant.
   */
  revoke(username: string, pattern: string): Promise<User> {
    return this.#user({ type: "revoke", username, pattern });
  }

  /**
   * Sets an account's password. Tokens already issued stay valid until they expire.
   *
   * @param username - The account.
   * @param password - The new password.
   * @returns The account.
   * @throws PopSubError with code `user_not_found`, or `bad_request` for a
   *   password that is too short or too long.
   */
  setPassword(username: string, password: string): Promise<User> {
    return this.#user({ type: "set_password", username, password });
  }

  /**
   * Enables or disables an account. Disabling closes its connections with
   * `account_disabled` (4009) and refuses its logins and tokens.
   *
   * @param username - The account.
   * @param enabled - Whether it may log in.
   * @returns The account as it now stands.
   * @throws PopSubError with code `user_not_found`, or `bad_request` for
   *   disabling the last enabled admin.
   */
  setEnabled(username: string, enabled: boolean): Promise<User> {
    return this.#user({ type: "set_enabled", username, enabled });
  }

  /**
   * Deletes an account and closes its connections with `account_disabled` (4009).
   *
   * @param username - The account.
   * @throws PopSubError with code `user_not_found`, `bad_request` for the
   *   last enabled admin, or of kind `protocol` when the server answers
   *   with something other than `user_deleted`.
   */
  async deleteUser(username: string): Promise<void> {
    const reply = await this.#send({ type: "delete_user", username });
    if (reply.type !== "user_deleted") {
      throw unexpected(reply, "user_deleted");
    }
  }

  /**
   * Sends a request answered by a `user` frame.
   *
   * @param request - The request.
   * @returns The account in the reply.
   * @throws PopSubError of kind `protocol` when the server answers with
   *   something other than `user`.
   */
  async #user(request: AdminRequest): Promise<User> {
    const reply = await this.#send(request);
    if (reply.type !== "user") {
      throw unexpected(reply, "user");
    }
    return fromWire(reply.user);
  }
}
