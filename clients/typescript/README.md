# PopSub TypeScript client

A client for [PopSub](../../README.md) that runs in browsers and in Node.js 22 or later. It uses only the global `WebSocket` and has no runtime dependencies.

It does what every PopSub client does (see [`clients/README.md`](../README.md)): logs in and refreshes its token, reconnects with jittered backoff and knows from the close code when not to, resubscribes from the next `seq` it expects so nothing still in the log is missed, resends confirmed publishes under the same request id and `message_id` so the server stores them once, acknowledges QoS 1 messages, and hands each `(topic, seq)` to the application at most once. It passes the shared scenario suite.

## Install

```bash
npm install popsub
```

To use a version that isn't released yet, build it from a checkout and install it from this folder:

```bash
cd clients/typescript
npm install
npm run build
cd /path/to/your/app
npm install /path/to/popsub/clients/typescript
```

## Usage

```ts
import { Client, PopSubError } from "popsub";

const client = await Client.connect(
  {
    url: "ws://127.0.0.1:8080",
    credentials: { username: "alice", password: "alice-password" },
    sessionId: "alice-laptop",
  },
  (event) => {
    if (event.type === "disconnected") {
      console.warn("disconnected", event.code, event.reconnecting);
    }
  },
);

const orders = await client.subscribe("orders", "resume");
const stored = await client.publishConfirmed("orders", "order 42 shipped");
console.log("stored as seq", stored.seq);

try {
  for await (const message of orders) {
    console.log(message.topic, message.seq, message.payload);
  }
} catch (error) {
  if (error instanceof PopSubError) {
    console.error(error.kind, error.code, error.closeCode);
  }
}
await client.close();
```

`examples/subscribe.ts` and `examples/publish.ts` are runnable versions of this, and [`examples/README.md`](../../examples/README.md) walks through running them against a local server.

## API

### `Client`

| Member                                           | What it does                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Client.connect(config, listener?)`              | Connects, logs in and authenticates, retrying with the reconnect policy for up to `requestTimeoutMs`. `listener` receives every event, including the first `connected`. Rejects with `invalid_credentials` (and other non-temporary refusals) at once, without retrying.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `subscribe(topic, from = "resume")`              | Resolves with a `Subscription` once the server confirms it. `topic` is a topic name or a wildcard pattern (below). `from` is `"resume"`, `"new"`, `{ last }`, `{ sinceMs }` or `{ seq }`. Subscribing to a topic or pattern again replaces the earlier subscription.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `subscribeShared(topic, group, from = "resume")` | Joins a shared subscription to a topic (below).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `unsubscribe(topic)`                             | Ends the subscription to a topic, or to a pattern and every topic joined through it, and tells the server. While disconnected it is queued and resolves at once.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `publish(topic, payload)`                        | QoS 0. Returns at once. Up to 10,000 publishes (and unsubscribes) are held while disconnected, oldest dropped first. Server refusals arrive as `serverError` events.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `publishConfirmed(topic, payload)`               | QoS 1. Resolves with `{ seq, messageId }` once stored. Resent after every reconnect until confirmed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `publishBytes(topic, bytes)`                     | QoS 0 for a `Uint8Array` body that isn't text. Otherwise the same as `publish`. See [Binary payloads](#binary-payloads).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `publishBytesConfirmed(topic, bytes)`            | QoS 1 for a `Uint8Array` body. Otherwise the same as `publishConfirmed`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `admin()`                                        | An `Admin` with the account calls `createUser`, `getUser`, `listUsers`, `setRole`, `grant`, `revoke`, `setPassword`, `setEnabled` and `deleteUser`, and the topic calls `listTopics`, `getTopic` and `purgeTopic`, which return `TopicInfo` (`name`, `firstSeq`, `lastSeq`, `messages`, `subscribers`). `getTopic` and `purgeTopic` reject with `topic_not_found` for a topic nothing was ever published to. None of these is queued or resent: while disconnected, or when the connection drops before the reply, they reject with `connect`. `backup(name?)` asks the server to back itself up and returns `BackupInfo` (`name`, `path`, `topics`, `records`, `bytes`, `users`) when the copy is done, which can take longer than the default `requestTimeoutMs`. |
| `on(listener)` / `off(listener)`                 | Registers or removes an event listener. `on` returns a function that removes it.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `close()`                                        | Stops the client: closes the socket, fails pending requests with `closed`, ends every subscription, clears every timer and listener. Resolves once stopped.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `stopped`                                        | `null` while running, then `{ code }` with the close code that stopped it (or `null`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |

### Wildcard subscriptions

In a pattern a whole level may be `+` (any one level) and the last level may be `#` (the rest). One subscription yields the messages of every matching topic the user may read, including topics created later, each under its own `topic` and `seq`. Order is kept within each topic, not across topics.

```ts
const orders = await client.subscribe("orders/#", "new");
for await (const message of orders) {
  console.log(message.topic, message.seq, message.payload);
}
```

Each topic the server joins for the pattern is tracked on its own (next expected `seq`, acknowledgements), so deduplication and gap-free resubscription work per topic. After a reconnect the client resubscribes those topics from where each left off and sends the pattern again, which picks up topics created while it was away from their first message. If the server ends one of the topics (an `unsubscribed` event naming it), only that topic stops. `client.unsubscribe("orders/#")`, `close()` on the subscription, or breaking out of the loop leaves the pattern and every topic joined through it.

Subscribing again to a pattern the client already holds replaces the subscription. The topics joined through it move to the new subscription and keep their next expected `seq`, so each later message arrives once, on the new subscription. The earlier subscription ends: it yields what it had already buffered and then `undefined`, and closing or dropping it doesn't unsubscribe anything.

### A durable session in a new process

A durable session (`sessionId`) outlives the process. When a new process connects with the same `sessionId`, the `connected` event lists the session's topics in `resumed` before the application has subscribed to anything. With a `sessionId` the client asks the server to hold those topics (`hold_resumed: true` in `auth`), and a server that honours it (it answers `held: true`) sends nothing on them until the client subscribes.

```ts
const client = await Client.connect({ url, credentials, sessionId: "alice-laptop", ackMode: "manual" });
const jobs = await client.subscribe("jobs", "resume");
const tasks = await client.subscribe("tasks/#");
```

When the topics are held, subscribing to one by name, or to a pattern that matches it, is an ordinary subscribe: `"resume"` starts one past the session's cursor, and the server joins a pattern's topic from that cursor whatever the pattern's `from`. Nothing is in flight for a topic the application never subscribes to, so it neither stalls nor is dead-lettered. After a reconnect the client subscribes again, from `"resume"`, to every topic it tracks that the server lists in `resumed`.

A server that doesn't know `hold_resumed` answers without `held` and starts sending what it kept straight away. Then, until the application subscribes to one of those topics, the client drops its messages without acknowledging them, so the server keeps them. Subscribing to the topic by name, or to a pattern that matches it, claims it: the subscribe replays the kept messages in order, and they are handed over from the `subscribed` reply on. For a pattern the client sends one more `subscribe` per claimed topic, by name with `from: "resume"`, because the server skips topics the connection already holds when it handles the pattern. A topic the application never subscribes to stays unclaimed: its messages wait on the server, where after `max_retries` the subscription stalls or the messages are dead-lettered, as for any subscriber that doesn't acknowledge. To drop a topic from the session, `unsubscribe` it.

### Shared subscriptions

Members of a group split a topic's messages: each message goes to one member. Subscribers outside the group still get everything.

```ts
const jobs = await client.subscribeShared("jobs", "workers", "new");
for await (const job of jobs) {
  await handle(job.payload);
  job.ack();
}
```

That example assumes `ackMode: "manual"`; in automatic mode `ack()` does nothing. A member gets only its share, not in strict `seq` order, and a message a member never acknowledged goes to another member when it disconnects, so workers must tolerate a repeat. Within one client, repeats are dropped by remembering the last 4096 `seq`s delivered. `from` only matters the first time a group is used on a topic; after that the group's position is on the server, and the client always rejoins with `resume` after a reconnect. A group needs an exact topic, not a pattern.

### `Subscription` and `Message`

A `Subscription` is an async iterable of `Message`s in `seq` order. `next()` resolves with the next message, or `undefined` once the subscription has ended and is drained. It ends when the server ends it (access revoked), when the application unsubscribes, or when the client stops. `close()`, or breaking out of `for await`, unsubscribes and drops the messages not yet taken. `buffered` is how many messages wait.

A `Message` has `topic`, `seq`, `messageId`, `payload`, `encoding`, `qos` and `publishedAtMs`, and `bytes()` returns its body as a `Uint8Array` (see [Binary payloads](#binary-payloads)). In manual mode, `ack()` acknowledges a QoS 1 message; in automatic mode and for QoS 0 it does nothing. Acknowledging twice is harmless.

### Binary payloads

A payload is always a string. For a body that isn't text, publish a `Uint8Array`:

```ts
client.publishBytes("frames", new Uint8Array([0x00, 0xff, 0xfe, 0x80]));
const stored = await client.publishBytesConfirmed("frames", bytes);

for await (const message of frames) {
  const body = message.bytes();
}
```

`publishBytes` and `publishBytesConfirmed` send the bytes as standard padded base64 in `payload` with `encoding: "base64"`, and otherwise behave exactly like `publish` and `publishConfirmed`: the same queue while disconnected, and the same resend under one request id and `message_id`. The server checks that the payload is base64 and carries it without decoding. Base64 adds about a third, and the server's payload limit (`max_payload_bytes`) counts the encoded string, so the largest body that fits is about three quarters of the limit.

On a received message, `payload` is the string as it arrived and `encoding` is `"base64"` for a message published as bytes or `null` for text. `bytes()` returns the decoded payload when the encoding is `base64`, and the payload's UTF-8 bytes otherwise, so it works for either kind. If a payload claims base64 and doesn't decode, the message is still delivered and `bytes()` throws a `PopSubError` of kind `protocol`. Encoding and decoding use a small strict codec in the package (padding required, no whitespace), not `Buffer`, so they work the same in browsers and Node.

### Events

`connected { resumed }`, `disconnected { code, reconnecting }`, `truncated { topic, firstAvailable }`, `stalled { topic, seq }`, `authExpiring { expiresAtMs }`, `authRefreshed`, `unsubscribed { topic }` (the server ended a subscription) and `serverError { code, message }` (an error that answers no request). A listener that throws doesn't stop the others; its error is rethrown from a microtask.

### Errors

Every failure is a `PopSubError` with `kind` (`server`, `closed`, `timeout`, `connect`, `protocol`, `config`), `code` for `server` errors (unknown codes read as `internal`), `retryAfter` for limits, `closeCode` for `closed`, and `cause` when it wraps another error. `isTemporary()` says whether retrying later might work.

### Configuration

| Field                | Default                                               | Meaning                                                                                                 |
| -------------------- | ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `url`                | required                                              | `ws://` or `wss://`                                                                                     |
| `credentials`        | required                                              | `{ username, password }`, or `{ token }` (a token can't be refreshed; the client stops when it expires) |
| `sessionId`          | `null`                                                | A durable session, 1 to 128 characters                                                                  |
| `cleanStart`         | `false`                                               | Discard the session's stored subscriptions, on the first connection only                                |
| `ackMode`            | `"auto"`                                              | `"auto"` or `"manual"`                                                                                  |
| `reconnect`          | `{ initialMs: 250, maxMs: 30000, maxAttempts: null }` | Each wait is random between zero and a ceiling that doubles from `initialMs` up to `maxMs`              |
| `requestTimeoutMs`   | `30000`                                               | Limit for connect, subscribe, unsubscribe, confirmed publishes and admin calls                          |
| `subscriptionBuffer` | `1024`                                                | Messages buffered per subscription before the client holds back (below)                                 |

## How it differs from the Rust client

The browser `WebSocket` API offers no way to stop reading a socket and doesn't show WebSocket pings, so backpressure works differently than in `crates/popsub_sdk`, and silence is detected with the protocol's `ping` request, as [`clients/README.md`](../README.md) describes for clients that can't see pings.

**Backpressure.** The Rust client stops reading the socket when a subscription's buffer is full. This client can't, so:

1. While a subscription holds `subscriptionBuffer` messages or more, automatic acknowledgements of new QoS 1 messages wait until the application takes those messages. The server stops sending to a subscription whose window (`limits.max_inflight`, 100 by default) is full, QoS 0 included, so QoS 1 traffic is paced by the application. Manual mode is paced the same way by the application's own acks.
2. If the buffer still reaches `subscriptionBuffer + max_inflight` messages, which only QoS 0 traffic can cause since no window paces it, the client drops the connection (a `disconnected` event with `code: null` and `reconnecting: true`), waits until the application has taken that buffer down to half of `subscriptionBuffer`, reconnects, and resubscribes that topic from the next `seq` it expects, so the server's log fills the gap. Messages trimmed from the log meanwhile are reported with `truncated`.

So each subscription holds fewer than `subscriptionBuffer + max_inflight` messages. While acknowledgements are held back, the server may resend those messages after its ack timeout; the client drops them as duplicates and doesn't acknowledge them, and after `max_retries` the server reports `stalled` and pauses the topic until the held acknowledgements go out.

**Silence detection.** Browsers and Node answer the server's WebSocket pings on their own but don't report them, so a quiet topic and a dead connection look the same. As `clients/README.md` asks of such clients, after `limits.heartbeat_secs` with nothing from the server the client sends the protocol's `ping` request with an id, and again after each further `heartbeat_secs` of silence. The server answers with `pong`, which also counts as client activity for its idle timeout. Any frame resets the silence. If nothing at all arrives within `idle_timeout_secs + heartbeat_secs`, the connection is treated as dead: the client drops it (a `disconnected` event with `code: null` and `reconnecting: true`) and reconnects.

**Numbers.** `seq` values are JavaScript numbers, exact up to 2^53.

## Scenarios

From the repository root, with `popsub-scenarios` already built:

```bash
(cd clients/typescript && npm install && npm run build)
target/debug/popsub-scenarios --driver "node clients/typescript/dist/driver.js"
target/debug/popsub-scenarios --driver "node clients/typescript/dist/driver.js" --scenario reconnect_resubscribes_without_gaps
```

`npm run scenarios` in this folder builds and runs the whole suite. The driver (`src/driver.ts`) speaks the protocol in [`clients/DRIVER.md`](../DRIVER.md). It is built to `dist/driver.js` but isn't exported from the package and isn't included in it.

## Development

```bash
npm install
npm run check
```

`npm run check` runs, in order, `typecheck` (`tsc` with `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes` and `noImplicitOverride`), `lint` (ESLint with typescript-eslint's strict and stylistic type-checked presets, no warnings allowed), `format:check` (Prettier) and `test` (Node's built-in test runner). The tests cover the backoff ceiling, byte payloads (the base64 codec, the frames byte and text publishes send, and `Message.bytes`), deduplication (by next expected `seq`, and by delivered `seq`s for shared subscriptions), the topic pattern matcher, close codes and configuration checks, check that a silent connection is pinged and that a `ping` left unanswered leads to a reconnect, and run a client against a small in-test WebSocket server to check how a new process claims a durable session's topics (nothing acknowledged or handed over before the application subscribes, by name or by pattern, and a claim sent again after a reconnect) and that subscribing again to a pattern moves its topics to the new subscription, and run one in a child process to check that a closed client leaves nothing that keeps Node running. `npm run format` rewrites files in place, and `npm run build` compiles to `dist/` with declaration files and source maps.
