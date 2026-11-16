/**
 * Tests for the protocol helpers and configuration checks: close codes and
 * whether they allow reconnecting, unknown error codes, frame parsing, and
 * rejected settings.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { resolveConfig, type ClientConfig } from "../src/config.js";
import { PopSubError } from "../src/errors.js";
import { closeCode, encodeFrame, errorCode, parseServerFrame, shouldReconnect } from "../src/protocol.js";

test("only the four final close codes rule out reconnecting", () => {
  const final = [4001, 4003, 4006, 4009];
  for (const raw of [1001, 1009, 1011, 1013, 4001, 4002, 4003, 4004, 4005, 4006, 4007, 4008, 4009]) {
    const code = closeCode(raw);
    assert.notEqual(code, null);
    if (code !== null) {
      assert.equal(shouldReconnect(code), !final.includes(raw));
    }
  }
});

test("close codes the server never sends read as no code", () => {
  assert.equal(closeCode(1000), null);
  assert.equal(closeCode(1006), null);
  assert.equal(closeCode(4999), null);
});

test("an unknown error code is treated like internal", () => {
  assert.equal(errorCode("not_authorized"), "not_authorized");
  assert.equal(errorCode("something_new"), "internal");
  assert.equal(errorCode(42), "internal");
});

test("frames that are malformed or of unknown types are ignored", () => {
  assert.equal(parseServerFrame("not json"), null);
  assert.equal(parseServerFrame("[1, 2]"), null);
  assert.equal(parseServerFrame(JSON.stringify({ type: "from_the_future" })), null);
  assert.equal(parseServerFrame(JSON.stringify({ type: "message", topic: "t" })), null);
  const frame = parseServerFrame(JSON.stringify({ type: "published", id: 7, message_id: "m", seq: 3 }));
  assert.deepEqual(frame, { type: "published", id: 7, message_id: "m", seq: 3 });
});

test("a frame carries its id only when one is given", () => {
  assert.equal(encodeFrame({ type: "ack", topic: "t", seq: 1 }), '{"type":"ack","topic":"t","seq":1}');
  assert.equal(
    encodeFrame({ type: "unsubscribe", topic: "t" }, 4),
    '{"type":"unsubscribe","topic":"t","id":4}',
  );
});

test("invalid settings are rejected with a config error", () => {
  const base: ClientConfig = { url: "ws://127.0.0.1:1", credentials: { token: "t" } };
  const cases: ClientConfig[] = [
    { ...base, url: "http://127.0.0.1:1" },
    { ...base, sessionId: "" },
    { ...base, requestTimeoutMs: 0 },
    { ...base, subscriptionBuffer: 0.5 },
    { ...base, reconnect: { maxAttempts: -1 } },
  ];
  for (const config of cases) {
    assert.throws(
      () => resolveConfig(config),
      (error: unknown) => error instanceof PopSubError && error.kind === "config",
    );
  }
  const resolved = resolveConfig(base);
  assert.equal(resolved.ackMode, "auto");
  assert.deepEqual(resolved.reconnect, { initialMs: 250, maxMs: 30_000, maxAttempts: null });
});

test("ping is sent with its id and pong is read as a reply", () => {
  assert.equal(encodeFrame({ type: "ping" }, 9), '{"type":"ping","id":9}');
  assert.deepEqual(parseServerFrame('{"type":"pong","id":9}'), { type: "pong", id: 9 });
});
