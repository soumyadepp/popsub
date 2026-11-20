/**
 * Tests for a durable session in a new process and for replacing a
 * wildcard subscription, against the scripted test server.
 *
 * The session tests have the server list topics in `authenticated.resumed`
 * and send what it kept for them straight after, as a real server does for
 * a session an earlier process held. They check that the client neither
 * hands over nor acknowledges those messages until the application
 * subscribes, that the subscribe (by name, or by a pattern that matches)
 * then delivers them from `subscribed.replay_from`, and that a claim left
 * unanswered by a dropped connection is sent again.
 *
 * The replacement tests subscribe to the same pattern twice and check that
 * the topics it had joined keep their next expected `seq` and feed the new
 * subscription, with the earlier one closed.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { Client } from "../src/index.js";
import { startFakeServer, type FakeServer, type Frame } from "./support/server.js";

/**
 * Builds a QoS 1 `message` frame.
 *
 * @param topic - The topic.
 * @param seq - Its position in the topic.
 * @returns The frame.
 */
function message(topic: string, seq: number): Frame {
  return {
    type: "message",
    topic,
    seq,
    message_id: `${topic}-${String(seq)}`,
    payload: `${topic} ${String(seq)}`,
    qos: 1,
    published_at_ms: 0,
  };
}

/**
 * Builds a `subscribed` frame with nothing to replay.
 *
 * @param request - The subscribe it answers, or `null` for one the server sends on its own.
 * @param topic - The topic or pattern.
 * @param replayFrom - The first `seq` the server will send.
 * @returns The frame.
 */
function subscribed(request: Frame | null, topic: string, replayFrom: number): Frame {
  const frame: Frame = { type: "subscribed", topic, replay_from: replayFrom, replay_to: replayFrom - 1 };
  return request === null ? frame : { ...frame, id: request.id };
}

/**
 * Waits until the server has handled everything the client sent so far and
 * the client has handled everything the server sent before its reply, by
 * making a confirmed publish and waiting for its `published`.
 *
 * @param client - The client.
 */
async function settle(client: Client): Promise<void> {
  await client.publishConfirmed("settle", "x");
}

/**
 * Lists the acknowledgements the server received.
 *
 * @param server - The server.
 * @returns Each as `topic:seq`, in order.
 */
function acks(server: FakeServer): string[] {
  return server
    .requests()
    .filter((request) => request.type === "ack")
    .map((request) => `${String(request.topic)}:${String(request.seq)}`);
}

/**
 * Lists the subscribes the server received.
 *
 * @param server - The server.
 * @returns Each as `topic from`, with `from` as JSON, in order.
 */
function subscribes(server: FakeServer): string[] {
  return server
    .requests()
    .filter((request) => request.type === "subscribe")
    .map((request) => `${String(request.topic)} ${JSON.stringify(request.from)}`);
}

/**
 * Connects a client holding the durable session `s`, acknowledging automatically.
 *
 * @param server - The server.
 * @returns The connected client.
 */
function connect(server: FakeServer): Promise<Client> {
  return Client.connect({
    url: server.url,
    credentials: { token: "t" },
    sessionId: "s",
    reconnect: { initialMs: 10, maxMs: 20 },
  });
}

test("a resumed topic's messages are kept unacknowledged until it is subscribed by name", async () => {
  const server = await startFakeServer({
    resumed: ["jobs"],
    kept: [message("jobs", 2)],
    respond: (request) =>
      request.type === "subscribe"
        ? [message("jobs", 3), subscribed(request, "jobs", 2), message("jobs", 2), message("jobs", 3)]
        : undefined,
  });
  const client = await connect(server);
  try {
    await settle(client);
    assert.deepEqual(acks(server), [], "a message was acknowledged before the application subscribed");
    const jobs = await client.subscribe("jobs", "resume");
    assert.equal((await jobs.next())?.seq, 2);
    assert.equal((await jobs.next())?.seq, 3);
    await settle(client);
    assert.equal(jobs.buffered, 0);
    assert.deepEqual(acks(server), ["jobs:2", "jobs:3"]);
    assert.deepEqual(subscribes(server), ['jobs "resume"']);
  } finally {
    await client.close();
    await server.close();
  }
});

test("a resumed topic no subscription covers stays unclaimed beside a pattern that doesn't match it", async () => {
  const server = await startFakeServer({
    resumed: ["jobs"],
    respond: (request) =>
      request.type === "subscribe" ? [subscribed(request, String(request.topic), 1)] : undefined,
  });
  const client = await connect(server);
  try {
    const other = await client.subscribe("other/#", "new");
    server.send(message("jobs", 1));
    await settle(client);
    assert.deepEqual(acks(server), []);
    assert.equal(other.buffered, 0);
    assert.deepEqual(subscribes(server), ['other/# "new"']);
  } finally {
    await client.close();
    await server.close();
  }
});

test("a pattern claims the resumed topics it matches with a subscribe by name from resume", async () => {
  const server = await startFakeServer({
    resumed: ["tasks/b", "jobs", "tasks/a"],
    kept: [message("tasks/a", 1), message("jobs", 1)],
    respond: (request) => {
      if (request.type !== "subscribe") {
        return undefined;
      }
      if (request.topic === "tasks/#") {
        return [subscribed(request, "tasks/#", 1), message("tasks/a", 2)];
      }
      if (request.topic === "tasks/a") {
        return [subscribed(request, "tasks/a", 1), message("tasks/a", 1), message("tasks/a", 2)];
      }
      return [subscribed(request, String(request.topic), 1)];
    },
  });
  const client = await connect(server);
  try {
    const tasks = await client.subscribe("tasks/#", "new");
    const first = await tasks.next();
    const second = await tasks.next();
    assert.deepEqual([first?.topic, first?.seq, second?.topic, second?.seq], ["tasks/a", 1, "tasks/a", 2]);
    await settle(client);
    assert.equal(tasks.buffered, 0);
    assert.deepEqual(subscribes(server), ['tasks/# "new"', 'tasks/a "resume"', 'tasks/b "resume"']);
    assert.deepEqual(acks(server), ["tasks/a:1", "tasks/a:2"]);
  } finally {
    await client.close();
    await server.close();
  }
});

test("a claim left unanswered is sent again after a reconnect although the topic is resumed", async () => {
  let asked = 0;
  const server = await startFakeServer({
    resumed: ["jobs"],
    kept: [message("jobs", 5)],
    respond: (request) => {
      if (request.type !== "subscribe") {
        return undefined;
      }
      asked += 1;
      return asked === 1 ? [] : [subscribed(request, "jobs", 5), message("jobs", 5)];
    },
  });
  const client = await connect(server);
  try {
    const pending = client.subscribe("jobs", { seq: 5 });
    await settle(client);
    assert.equal(asked, 1);
    server.drop();
    const jobs = await pending;
    assert.equal((await jobs.next())?.seq, 5);
    await settle(client);
    assert.deepEqual(subscribes(server), ['jobs {"seq":5}', 'jobs {"seq":5}']);
    assert.deepEqual(acks(server), ["jobs:5"]);
  } finally {
    await client.close();
    await server.close();
  }
});

test("subscribing again to a pattern moves its topics to the new subscription once each", async () => {
  let asked = 0;
  const server = await startFakeServer({
    respond: (request) => {
      if (request.type !== "subscribe") {
        return undefined;
      }
      asked += 1;
      const accepted = subscribed(request, "orders/#", 1);
      return asked === 1 ? [accepted, subscribed(null, "orders/a", 1), message("orders/a", 1)] : [accepted];
    },
  });
  const client = await Client.connect({ url: server.url, credentials: { token: "t" } });
  try {
    const earlier = await client.subscribe("orders/#", "new");
    assert.equal((await earlier.next())?.seq, 1);
    const later = await client.subscribe("orders/#", "new");
    assert.equal(await earlier.next(), undefined, "the earlier subscription didn't end");
    server.send(message("orders/a", 1));
    server.send(message("orders/a", 2));
    const next = await later.next();
    assert.deepEqual([next?.topic, next?.seq], ["orders/a", 2]);
    await settle(client);
    assert.equal(later.buffered, 0);
    assert.deepEqual(acks(server), ["orders/a:1", "orders/a:1", "orders/a:2"]);
  } finally {
    await client.close();
    await server.close();
  }
});

test("closing the earlier subscription to a replaced pattern loses nothing on joined topics", async () => {
  let asked = 0;
  const server = await startFakeServer({
    respond: (request) => {
      if (request.type !== "subscribe") {
        return undefined;
      }
      asked += 1;
      const accepted = subscribed(request, "orders/#", 1);
      return asked === 1 ? [accepted, subscribed(null, "orders/a", 1), message("orders/a", 1)] : [accepted];
    },
  });
  const client = await Client.connect({ url: server.url, credentials: { token: "t" } });
  try {
    const earlier = await client.subscribe("orders/#", "new");
    assert.equal((await earlier.next())?.seq, 1);
    const later = await client.subscribe("orders/#", "new");
    earlier.close();
    server.send(message("orders/a", 2));
    server.send(message("orders/b", 1));
    const seen = [await later.next(), await later.next()].map(
      (delivered) => `${String(delivered?.topic)}:${String(delivered?.seq)}`,
    );
    assert.deepEqual(seen, ["orders/a:2", "orders/b:1"]);
    await settle(client);
    assert.ok(
      !server.received().includes("unsubscribe"),
      "closing the earlier subscription unsubscribed something",
    );
  } finally {
    await client.close();
    await server.close();
  }
});
