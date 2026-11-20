/**
 * Tests for byte payloads: the base64 helper, the frames `publishBytes`
 * and `publishBytesConfirmed` send (base64 in `payload` with
 * `encoding: "base64"`, where text publishes carry no `encoding`), a
 * confirmed byte publish resent unchanged after a reconnect, and
 * `Message.bytes` for a base64 message, a text message and a payload that
 * claims base64 and isn't, against the scripted test server.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { decodeBase64, encodeBase64 } from "../src/base64.js";
import { Client, PopSubError } from "../src/index.js";
import { publishFrame } from "../src/protocol.js";
import { startFakeServer, type FakeServer, type Frame } from "./support/server.js";

/** Bytes that aren't valid UTF-8. */
const BINARY = new Uint8Array([0x00, 0xff, 0xfe, 0x80]);

/** `BINARY` in standard padded base64. */
const BINARY_BASE64 = "AP/+gA==";

/**
 * Connects a client that reconnects quickly.
 *
 * @param server - The server.
 * @returns The connected client.
 */
function connect(server: FakeServer): Promise<Client> {
  return Client.connect({
    url: server.url,
    credentials: { token: "t" },
    reconnect: { initialMs: 10, maxMs: 20 },
  });
}

/**
 * Builds a QoS 0 `message` frame on the topic `blobs`.
 *
 * @param seq - Its position in the topic.
 * @param payload - The payload string.
 * @param encoding - The `encoding` field, or `undefined` to leave it out.
 * @returns The frame.
 */
function delivery(seq: number, payload: string, encoding?: string | null): Frame {
  return {
    type: "message",
    topic: "blobs",
    seq,
    message_id: `m-${String(seq)}`,
    payload,
    qos: 0,
    published_at_ms: 0,
    ...(encoding === undefined ? {} : { encoding }),
  };
}

/**
 * Lists the publishes the server received.
 *
 * @param server - The server.
 * @returns The `publish` requests, in order.
 */
function publishes(server: FakeServer): Frame[] {
  return server.requests().filter((request) => request.type === "publish");
}

test("base64 encodes with padding and decodes back, for every length up to 64", () => {
  assert.equal(encodeBase64(new Uint8Array()), "");
  assert.equal(encodeBase64(BINARY), BINARY_BASE64);
  assert.equal(encodeBase64(new TextEncoder().encode("fo")), "Zm8=");
  assert.equal(encodeBase64(new TextEncoder().encode("foo")), "Zm9v");
  for (let length = 0; length <= 64; length += 1) {
    const bytes = Uint8Array.from({ length }, (_, i) => (i * 37 + 251) & 0xff);
    const encoded = encodeBase64(bytes);
    assert.equal(encoded, Buffer.from(bytes).toString("base64"));
    assert.deepEqual(decodeBase64(encoded), bytes);
  }
});

test("base64 decoding refuses what a standard encoder never writes", () => {
  for (const invalid of [
    "AP/+gA",
    "AP/+gA=",
    "AP_-gA==",
    "AP/+ gA==",
    "AP/+gA==\n",
    "A===",
    "=AAA",
    "AA=A",
    "AP/+gB==",
    "Zm9=",
    "not base64!",
    "Zm9véAAA",
  ]) {
    assert.equal(decodeBase64(invalid), null, `${JSON.stringify(invalid)} was accepted`);
  }
});

test("a publish frame carries an encoding only for bytes", () => {
  assert.deepEqual(publishFrame("t", "hello", null), { type: "publish", topic: "t", payload: "hello" });
  assert.deepEqual(publishFrame("t", BINARY_BASE64, "base64"), {
    type: "publish",
    topic: "t",
    payload: BINARY_BASE64,
    encoding: "base64",
  });
});

test("byte publishes send base64 with the encoding and text publishes send none", async () => {
  const server = await startFakeServer();
  const client = await connect(server);
  try {
    client.publishBytes("blobs", BINARY);
    client.publish("notes", "hello");
    const stored = await client.publishBytesConfirmed("blobs", BINARY);
    await client.publishConfirmed("notes", "hello");
    const [bytes, text, bytesConfirmed, textConfirmed] = publishes(server);
    assert.deepEqual(bytes, { type: "publish", topic: "blobs", payload: BINARY_BASE64, encoding: "base64" });
    assert.deepEqual(text, { type: "publish", topic: "notes", payload: "hello" });
    assert.equal(bytesConfirmed?.payload, BINARY_BASE64);
    assert.equal(bytesConfirmed.encoding, "base64");
    assert.equal(bytesConfirmed.qos, 1);
    assert.equal(bytesConfirmed.message_id, stored.messageId);
    assert.equal(textConfirmed?.payload, "hello");
    assert.equal(textConfirmed.qos, 1);
    assert.equal(Object.hasOwn(textConfirmed, "encoding"), false);
  } finally {
    await client.close();
    await server.close();
  }
});

test("a confirmed byte publish is resent unchanged after a reconnect", async () => {
  let answering = false;
  const server = await startFakeServer({
    respond: (request) => (request.type === "publish" && !answering ? [] : undefined),
  });
  const client = await connect(server);
  try {
    const stored = client.publishBytesConfirmed("blobs", BINARY);
    while (publishes(server).length < 1) {
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    answering = true;
    server.drop();
    const published = await stored;
    const sent = publishes(server);
    assert.equal(sent.length, 2);
    assert.deepEqual(sent[1], sent[0]);
    assert.equal(sent[0]?.encoding, "base64");
    assert.equal(sent[0].payload, BINARY_BASE64);
    assert.equal(sent[0].message_id, published.messageId);
  } finally {
    await client.close();
    await server.close();
  }
});

test("a byte publish made while disconnected is queued and sent with its encoding", async () => {
  const server = await startFakeServer();
  const client = await connect(server);
  try {
    const disconnected = new Promise<void>((resolve) => {
      client.on((event) => {
        if (event.type === "disconnected") {
          resolve();
        }
      });
    });
    server.drop();
    await disconnected;
    client.publishBytes("blobs", BINARY);
    await client.publishConfirmed("settle", "x");
    assert.deepEqual(
      publishes(server).filter((request) => request.topic === "blobs"),
      [{ type: "publish", topic: "blobs", payload: BINARY_BASE64, encoding: "base64" }],
    );
  } finally {
    await client.close();
    await server.close();
  }
});

test("a message's bytes are the decoded payload for base64 and the UTF-8 for text", async () => {
  const text = "café \u{10348}";
  const server = await startFakeServer({
    respond: (request) =>
      request.type === "subscribe"
        ? [
            { type: "subscribed", id: request.id, topic: "blobs", replay_from: 1, replay_to: 0 },
            delivery(1, BINARY_BASE64, "base64"),
            delivery(2, text),
            delivery(3, BINARY_BASE64, null),
            delivery(4, "", "base64"),
          ]
        : undefined,
  });
  const client = await connect(server);
  try {
    const blobs = await client.subscribe("blobs", "new");
    const binary = await blobs.next();
    assert.equal(binary?.encoding, "base64");
    assert.equal(binary.payload, BINARY_BASE64);
    assert.deepEqual(binary.bytes(), BINARY);
    const plain = await blobs.next();
    assert.equal(plain?.encoding, null);
    assert.equal(plain.payload, text);
    assert.deepEqual(plain.bytes(), new Uint8Array(Buffer.from(text, "utf8")));
    const unmarked = await blobs.next();
    assert.equal(unmarked?.encoding, null);
    assert.deepEqual(unmarked.bytes(), new Uint8Array(Buffer.from(BINARY_BASE64, "utf8")));
    const empty = await blobs.next();
    assert.equal(empty?.encoding, "base64");
    assert.deepEqual(empty.bytes(), new Uint8Array());
  } finally {
    await client.close();
    await server.close();
  }
});

test("a payload that claims base64 and isn't is delivered, and its bytes throw a protocol error", async () => {
  const server = await startFakeServer({
    respond: (request) =>
      request.type === "subscribe"
        ? [
            { type: "subscribed", id: request.id, topic: "blobs", replay_from: 1, replay_to: 0 },
            delivery(1, "not base64!", "base64"),
            delivery(2, BINARY_BASE64, "base64"),
          ]
        : undefined,
  });
  const client = await connect(server);
  try {
    const blobs = await client.subscribe("blobs", "new");
    const broken = await blobs.next();
    assert.equal(broken?.seq, 1);
    assert.equal(broken.payload, "not base64!");
    assert.equal(broken.encoding, "base64");
    assert.throws(
      () => broken.bytes(),
      (error: unknown) => error instanceof PopSubError && error.kind === "protocol",
    );
    const next = await blobs.next();
    assert.equal(next?.seq, 2);
    assert.deepEqual(next.bytes(), BINARY);
  } finally {
    await client.close();
    await server.close();
  }
});
