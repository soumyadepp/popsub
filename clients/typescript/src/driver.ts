/**
 * The scenario driver for the TypeScript client: reads commands from
 * standard input and writes responses and events to standard output, one
 * JSON object per line, as `clients/DRIVER.md` describes. Each command is
 * handled concurrently and answered when it completes. Not part of the
 * library's public entry point.
 *
 * Run it from the repository root with `node clients/typescript/dist/driver.js`.
 *
 * @module
 */

import { createInterface } from "node:readline";

import type { User } from "./admin.js";
import { Client, type StartFrom } from "./client.js";
import type { ClientConfig } from "./config.js";
import { PopSubError } from "./errors.js";
import type { ClientEvent } from "./events.js";
import type { Access, RoleSpec } from "./protocol.js";
import type { Message } from "./subscription.js";

/** A JSON object. */
type Json = Record<string, unknown>;

/** A command from the harness. */
type Command = Json & { id?: unknown; op?: unknown };

/** What the driver keeps between commands. */
interface State {
  /** The client, once connected. */
  client: Client | null;
  /** Messages reported and not yet acknowledged, by `topic` and `seq`, for `ack`. */
  held: Map<string, Message>;
  /** Topics the harness unsubscribed from, whose ending isn't reported. */
  leaving: Set<string>;
}

/**
 * Writes one line to standard output.
 *
 * @param value - The object to write.
 */
function emit(value: Json): void {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

/**
 * Builds the key of a held message.
 *
 * @param topic - The topic.
 * @param seq - The message's `seq`.
 * @returns The key.
 */
function heldKey(topic: string, seq: number): string {
  return `${String(seq)}:${topic}`;
}

/**
 * Builds an error to send back for a failed command.
 *
 * @param kind - The error kind.
 * @param message - What went wrong.
 * @returns The error.
 */
function failure(kind: "protocol" | "config", message: string): PopSubError {
  return new PopSubError(kind, message);
}

/**
 * Reads a string argument.
 *
 * @param source - The command or its params.
 * @param key - The argument's name.
 * @returns The string, or an empty string when it is missing.
 */
function text(source: Json, key: string): string {
  const value = source[key];
  return typeof value === "string" ? value : "";
}

/**
 * Reads a whole number argument.
 *
 * @param source - The object holding it.
 * @param key - The argument's name.
 * @returns The number, or `null` when it is missing or not a number.
 */
function integer(source: Json, key: string): number | null {
  const value = source[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * Reads an object argument.
 *
 * @param source - The object holding it.
 * @param key - The argument's name.
 * @returns The object, or an empty one when it is missing.
 */
function object(source: Json, key: string): Json {
  const value = source[key];
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Json) : {};
}

/**
 * Converts the protocol's `StartFrom` to the library's.
 *
 * @param raw - The `from` argument as the harness wrote it.
 * @returns The start position.
 * @throws PopSubError of kind `config` when it isn't a known shape.
 */
function startFrom(raw: unknown): StartFrom {
  if (raw === "resume" || raw === "new") {
    return raw;
  }
  if (typeof raw === "object" && raw !== null) {
    const fields = raw as Json;
    for (const [wire, key] of [
      ["last", "last"],
      ["since_ms", "sinceMs"],
      ["seq", "seq"],
    ] as const) {
      const value = integer(fields, wire);
      if (value !== null) {
        return { [key]: value } as StartFrom;
      }
    }
  }
  throw failure("config", `unknown from: ${JSON.stringify(raw)}`);
}

/**
 * Describes an account in its wire form.
 *
 * @param user - The account.
 * @returns The wire form.
 */
function userJson(user: User): Json {
  return {
    username: user.username,
    role: user.role,
    enabled: user.enabled,
    created_at_ms: user.createdAtMs,
  };
}

/**
 * Describes a client event as a driver event.
 *
 * @param event - The event.
 * @returns The driver event.
 */
function eventJson(event: ClientEvent): Json {
  switch (event.type) {
    case "connected":
      return { event: "connected", resumed: event.resumed };
    case "disconnected":
      return { event: "disconnected", code: event.code, reconnecting: event.reconnecting };
    case "truncated":
      return { event: "truncated", topic: event.topic, first_available: event.firstAvailable };
    case "stalled":
      return { event: "stalled", topic: event.topic, seq: event.seq };
    case "authExpiring":
      return { event: "auth_expiring", expires_at_ms: event.expiresAtMs };
    case "authRefreshed":
      return { event: "auth_refreshed" };
    case "unsubscribed":
      return { event: "unsubscribed", topic: event.topic };
    case "serverError":
      return { event: "server_error", code: event.code, message: event.message };
  }
}

/**
 * Describes an error as the driver protocol's `error` object.
 *
 * @param error - The error.
 * @returns The `error` object.
 */
function errorJson(error: unknown): Json {
  if (!(error instanceof PopSubError)) {
    return { kind: "protocol", message: error instanceof Error ? error.message : String(error) };
  }
  switch (error.kind) {
    case "server":
      return { kind: "server", code: error.code, message: error.message };
    case "closed":
      return { kind: "closed", close_code: error.closeCode, message: error.message };
    default:
      return { kind: error.kind, message: error.message };
  }
}

/**
 * Connects the client and starts forwarding its events.
 *
 * @param state - The driver's state.
 * @param command - The `connect` command.
 * @returns An empty result.
 */
async function connect(state: State, command: Command): Promise<Json> {
  if (state.client !== null) {
    throw failure("protocol", "already connected");
  }
  const reconnect = object(command, "reconnect");
  const sessionId = command.session_id;
  const config: ClientConfig = {
    url: text(command, "url"),
    credentials: { username: text(command, "username"), password: text(command, "password") },
    sessionId: typeof sessionId === "string" ? sessionId : null,
    cleanStart: command.clean_start === true,
    ackMode: command.ack_mode === "manual" ? "manual" : "auto",
    reconnect: {
      initialMs: integer(reconnect, "initial_ms") ?? 250,
      maxMs: integer(reconnect, "max_ms") ?? 30_000,
      maxAttempts: integer(reconnect, "max_attempts"),
    },
    requestTimeoutMs: integer(command, "request_timeout_ms") ?? 30_000,
  };
  state.client = await Client.connect(config, (event) => {
    emit(eventJson(event));
  });
  return {};
}

/**
 * Reports a subscription's messages until it ends, then reports the end
 * unless the harness asked for it.
 *
 * @param state - The driver's state.
 * @param topic - The topic.
 * @param messages - The subscription.
 */
async function forward(state: State, topic: string, messages: AsyncIterable<Message>): Promise<void> {
  for await (const message of messages) {
    emit({
      event: "message",
      topic: message.topic,
      seq: message.seq,
      payload: message.payload,
      message_id: message.messageId,
      qos: message.qos,
    });
    if (message.qos === 1) {
      state.held.set(heldKey(message.topic, message.seq), message);
    }
  }
  if (!state.leaving.has(topic)) {
    emit({ event: "subscription_ended", topic });
  }
}

/**
 * Calls an admin method.
 *
 * @param client - The client.
 * @param command - The `admin` command.
 * @returns `{ result }`: an account, a list of them, or `null`.
 */
async function admin(client: Client, command: Command): Promise<Json> {
  const params = object(command, "params");
  const username = text(params, "username");
  const role = params.role as RoleSpec;
  const calls = client.admin();
  switch (command.method) {
    case "create_user":
      return { result: userJson(await calls.createUser(username, text(params, "password"), role)) };
    case "get_user":
      return { result: userJson(await calls.getUser(username)) };
    case "list_users":
      return { result: (await calls.listUsers()).map(userJson) };
    case "set_role":
      return { result: userJson(await calls.setRole(username, role)) };
    case "grant":
      return {
        result: userJson(await calls.grant(username, text(params, "pattern"), params.access as Access)),
      };
    case "revoke":
      return { result: userJson(await calls.revoke(username, text(params, "pattern"))) };
    case "set_password":
      return { result: userJson(await calls.setPassword(username, text(params, "password"))) };
    case "set_enabled":
      return { result: userJson(await calls.setEnabled(username, params.enabled !== false)) };
    case "delete_user":
      await calls.deleteUser(username);
      return { result: null };
    default:
      throw failure("protocol", `unknown admin method ${JSON.stringify(command.method)}`);
  }
}

/**
 * Carries out one command.
 *
 * @param state - The driver's state.
 * @param command - The command.
 * @returns The response's result fields.
 */
async function handle(state: State, command: Command): Promise<Json> {
  const op = command.op;
  if (op === "connect") {
    return connect(state, command);
  }
  const client = state.client;
  if (client === null) {
    throw failure("protocol", "connect first");
  }
  const topic = text(command, "topic");
  switch (op) {
    case "subscribe": {
      const from = startFrom(command.from);
      state.leaving.delete(topic);
      const subscription = await client.subscribe(topic, from);
      void forward(state, topic, subscription);
      return {};
    }
    case "unsubscribe":
      state.leaving.add(topic);
      await client.unsubscribe(topic);
      return {};
    case "publish":
      client.publish(topic, text(command, "payload"));
      return {};
    case "publish_confirmed": {
      const published = await client.publishConfirmed(topic, text(command, "payload"));
      return { seq: published.seq, message_id: published.messageId };
    }
    case "ack": {
      const key = heldKey(topic, integer(command, "seq") ?? -1);
      const message = state.held.get(key);
      if (message === undefined) {
        throw failure("protocol", "no such message was reported");
      }
      state.held.delete(key);
      message.ack();
      return {};
    }
    case "admin":
      return admin(client, command);
    case "close":
      await client.close();
      return {};
    default:
      throw failure("protocol", `unknown op ${JSON.stringify(op)}`);
  }
}

/**
 * Answers one command and writes its response.
 *
 * @param state - The driver's state.
 * @param command - The command.
 */
async function respond(state: State, command: Command): Promise<void> {
  const id = command.id ?? null;
  try {
    const result = await handle(state, command);
    emit({ ...result, id, ok: true });
  } catch (error) {
    emit({ id, ok: false, error: errorJson(error) });
  }
}

/**
 * Closes the client, flushes standard output and exits.
 *
 * @param state - The driver's state.
 */
async function shutdown(state: State): Promise<void> {
  await state.client?.close();
  process.stdout.write("", () => process.exit(0));
}

/** Reads commands until standard input closes or `exit`. */
function main(): void {
  const state: State = { client: null, held: new Map(), leaving: new Set() };
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  let exiting = false;
  input.on("line", (line) => {
    if (exiting || line.trim() === "") {
      return;
    }
    let command: Command;
    try {
      const parsed: unknown = JSON.parse(line);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        throw new Error("not an object");
      }
      command = parsed as Command;
    } catch {
      process.stderr.write(`ignoring a line that isn't a JSON object: ${line}\n`);
      return;
    }
    if (command.op === "exit") {
      exiting = true;
      emit({ id: command.id ?? null, ok: true });
      input.close();
      return;
    }
    void respond(state, command);
  });
  input.on("close", () => {
    void shutdown(state);
  });
}

main();
