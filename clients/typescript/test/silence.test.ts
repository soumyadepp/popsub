/**
 * Tests for silence detection against the test server, which never sends
 * WebSocket pings and announces `heartbeat_secs = 1` and
 * `idle_timeout_secs = 1`: a silent connection gets a `ping` after a
 * second, a `pong` keeps it up, and a `ping` left unanswered makes the
 * client give the connection up after two seconds and reconnect.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { Client, type ClientEvent } from "../src/index.js";
import { startFakeServer } from "./support/server.js";

/**
 * Waits until a condition holds, checking every 20 ms.
 *
 * @param condition - The condition.
 * @param withinMs - The longest to wait.
 * @returns True if it held in time.
 */
async function waitFor(condition: () => boolean, withinMs: number): Promise<boolean> {
  const deadline = Date.now() + withinMs;
  while (!condition()) {
    if (Date.now() >= deadline) {
      return false;
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  return true;
}

/**
 * Waits for a time.
 *
 * @param ms - How long.
 * @returns A promise that settles after it.
 */
function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

test("a silent connection is pinged and stays up while pongs come back", async () => {
  const server = await startFakeServer({ heartbeatSecs: 1, idleTimeoutSecs: 1 });
  const events: ClientEvent[] = [];
  const client = await Client.connect({ url: server.url, credentials: { token: "t" } }, (event) =>
    events.push(event),
  );
  try {
    await delay(3500);
    const pings = server.received().filter((type) => type === "ping").length;
    assert.ok(pings >= 2, `expected at least two pings in 3.5 s of silence, got ${String(pings)}`);
    assert.deepEqual(
      events.map((event) => event.type),
      ["connected"],
    );
    assert.equal(server.authenticated(), 1);
  } finally {
    await client.close();
    await server.close();
  }
});

test("a ping without a pong makes the client drop the connection and reconnect", async () => {
  const server = await startFakeServer({ heartbeatSecs: 1, idleTimeoutSecs: 1 });
  server.answerPings(false);
  const events: ClientEvent[] = [];
  const started = Date.now();
  const client = await Client.connect(
    { url: server.url, credentials: { token: "t" }, reconnect: { initialMs: 20, maxMs: 50 } },
    (event) => events.push(event),
  );
  try {
    assert.ok(
      await waitFor(() => events.some((event) => event.type === "disconnected"), 5000),
      "the client never gave up the silent connection",
    );
    const elapsed = Date.now() - started;
    assert.ok(elapsed >= 1900, `gave up after ${String(elapsed)} ms, before idle + heartbeat`);
    assert.deepEqual(
      events.find((event) => event.type === "disconnected"),
      {
        type: "disconnected",
        code: null,
        reconnecting: true,
      },
    );
    const received = server.received();
    assert.ok(received.includes("ping"), `no ping was sent: ${received.join(",")}`);
    assert.ok(await waitFor(() => server.authenticated() === 2, 2000), "the client didn't reconnect");
    assert.ok(await waitFor(() => events.filter((event) => event.type === "connected").length === 2, 1000));
  } finally {
    await client.close();
    await server.close();
  }
});
