# PopSub design

| | |
|---|---|
| Status | Describes `main` as of November 2026, plus the proposed direction |
| Owner | Soumyadeep Ghosh |
| Scope | The broker, its WebSocket protocol, persistence and delivery guarantees |

## 1. Summary

PopSub is a single-process publish/subscribe broker written in Rust. Clients connect over WebSocket, log in, and then subscribe to topics or publish to them using a small JSON protocol. Messages can be sent fire-and-forget (QoS 0) or with acknowledgements and retries (QoS 1). Recent messages are stored per topic in an embedded `sled` database and replayed to new subscribers.

This document describes how the system works today, where its guarantees hold and where they don't, and the changes needed before other people can depend on it.

## 2. Goals and non-goals

**Goals**

- Small enough to read in an afternoon. A newcomer should be able to trace a message from socket to subscriber without a diagram.
- One binary, no external services. `cargo run` and a directory on disk is the whole deployment.
- Honest delivery semantics: every guarantee the README makes should be backed by a test.
- A protocol any language can speak with a WebSocket library and a JSON parser.

**Non-goals (for now)**

- Clustering or replication. A single node is the unit of deployment. Section 13 discusses how this could change.
- Competing with Kafka, NATS or Redis Streams on throughput.
- Exactly-once delivery. At-least-once plus idempotent consumers is the intended model.
- Binary payloads. Payloads are strings.

## 3. Architecture

### 3.1 Crates

```
                 popsub (CLI: server, client)
                   |
        +----------+-------------------------+
        |                                    |
 popsub_transport (WebSocket, protocol, auth) |
        |                                    |
 popsub_broker (topics, routing, QoS 1 retries)
        |              |
 popsub_persistence   popsub_client (per-connection record)
        |
 popsub_utils (errors, logging)       popsub_config (settings loader)
```

| Crate | Responsibility |
|---|---|
| `popsub` | Binary. Parses the CLI, loads config, starts the broker, the retry loop and the WebSocket server. Also contains a small demo client. |
| `popsub_transport` | Accepts connections, parses `ClientMessage` JSON, enforces login then auth, issues and checks JWTs, and turns requests into broker calls. |
| `popsub_broker` | The engine: topics, subscriber sets, publishing, QoS 1 pending table and retry loop, replay on subscribe. Synchronous API. |
| `popsub_persistence` | `sled` store of recent messages per topic, with a TTL and a per-topic cap. |
| `popsub_client` | `Client`, the server-side record of one connection: id, outbound channel, authenticated flag. Despite the name it is not a client library. |
| `popsub_config` | Loads `Settings` from `config/default.toml` and environment variables, falling back to defaults. |
| `popsub_utils` | `PopSubError`, the shared `Result` alias, and logging setup. |

Dependencies point downwards only. The broker knows nothing about WebSockets beyond the message type it pushes into each client's channel.

### 3.2 Runtime

```
 socket --> connection task --(lock)--> Broker --> client.sender (unbounded mpsc) --> send loop --> socket
                                          ^
                    retry loop (every 1s) |
                                          v
                                        sled
```

- **Connection task.** One per socket. Reads frames, parses JSON outside the lock, then takes the broker lock to update the client and apply the request.
- **Send loop.** One per socket. Drains the client's channel and writes frames. When the socket fails it cleans the client up.
- **Broker.** A single `Arc<std::sync::Mutex<Broker>>` shared by every task.
- **Retry loop.** A background task that wakes every second, collects QoS 1 messages whose ACK is overdue, and resends or drops them.

## 4. Protocol

All frames are JSON text. Client messages are tagged with `type`.

### 4.1 Client to server

| `type` | Fields | Notes |
|---|---|---|
| `login` | `username`, `password` | Checked against the single configured user. Returns a JWT. |
| `auth` | `token` | Must succeed before anything below is accepted. A bad token closes the connection. |
| `subscribe` | `topic` | Creates the topic if needed and replays stored messages. |
| `unsubscribe` | `topic` | |
| `publish` | `topic`, `payload`, optional `message_id`, optional `qos` (0 or 1) | Broker sets `timestamp` and generates `message_id` when missing. |
| `ack` | `message_id` | Confirms a QoS 1 delivery. |

### 4.2 Server to client

| Frame | Shape |
|---|---|
| Login result | `{"type":"login_response","token":"..."}` |
| Auth result | `{"type":"authenticated"}` |
| Error | `{"type":"error","message":"..."}` |
| Delivered message | `{"topic":"...","payload":"...","timestamp":0,"message_id":"...","qos":0}` |

**Inconsistency to fix:** delivered messages are the broker's `Message` struct serialised directly, so they carry no `type` field. `ServerMessage::Message` exists in the transport crate but is never used. Clients currently have to detect deliveries by the absence of `type`. Section 12 proposes wrapping deliveries in `ServerMessage::Message`.

### 4.3 Session order

1. Connect.
2. `login`. Wrong credentials produce an error and the connection stays open.
3. `auth` with the token. Failure closes the connection.
4. Any other request. Requests before successful `auth` produce an error and close the connection.

## 5. Message lifecycle

### 5.1 Publish

1. Transport parses the frame and builds a `Message`, using the client's `message_id` if given.
2. Under the broker lock: set the timestamp, generate an id if missing, insert into `pending_acks` when `qos == 1`, write the topic and payload to `sled`, serialise, and push the frame into every subscriber's channel.
3. If the topic has no subscribers the message is still stored, and a QoS 1 message still sits in `pending_acks` until the retry loop gives up on it.

### 5.2 Subscribe and replay

1. Under the broker lock: add the client to the topic's subscriber set, creating the topic if needed.
2. Load every stored message for the topic (after deleting expired ones) and push each to the client as QoS 0 with a fresh `message_id`.

Replay happens on every subscribe, so a client that reconnects receives the whole retained history again.

### 5.3 Acknowledgement and retry

- `ack` removes the entry from `pending_acks`, whichever subscriber sent it.
- Every second the retry loop finds entries older than `ack_timeout_ms`. Entries that have hit `max_retries` are dropped with a warning; the rest are resent to every current subscriber of the topic.

## 6. Delivery semantics

| | What the code guarantees today |
|---|---|
| QoS 0 | At most once, to subscribers connected at publish time. Plus replay of retained history on every subscribe. |
| QoS 1, one subscriber that stays connected | At least once, within `max_retries` attempts. |
| QoS 1, several subscribers | Not at least once. The first ACK from any subscriber clears the message for all of them. Retries go to every subscriber, including those that already acknowledged. |
| QoS 1, subscriber disconnects | Not at least once. The client record and its subscriptions are deleted on disconnect, and a reconnect gets a new identity. |
| QoS 1, broker restarts | Not at least once. `pending_acks` lives only in memory. |
| Ordering | Per publisher and topic, while the lock serialises everything. Retries and replays can arrive out of order. |
| Duplicates | Possible and not detectable: replays get fresh ids, and retries reuse the id but go to everyone. |

The README and the portfolio both describe PopSub as at-least-once. That is only accurate for the single-subscriber, always-connected case, and should be stated that way until the phase 1 work lands.

## 7. Persistence

- One `sled` tree per topic. Key: 20-digit millisecond timestamp, underscore, UUID, so iteration is chronological. Value: JSON `{topic, payload, timestamp}`.
- The message id and QoS level are not stored.
- On write, if the tree exceeds `max_messages_per_topic`, the oldest keys are deleted.
- On read, keys older than `ttl_seconds` are deleted first.
- `Broker::new` uses `Persistence::default()`: path `pubsub_db` relative to the working directory, TTL 3600 seconds, cap 1000. The `message_ttl_secs` setting is loaded but never passed to persistence, and the path isn't configurable.
- `sled` 0.34 has been the stable line for years while its successor stays in alpha. It is fine for this scale, but it is a dependency risk worth tracking.

## 8. Concurrency

- Every operation takes the same `std::sync::Mutex`. Publishes, subscribes, ACKs, connects, disconnects and the retry loop all queue behind each other.
- `sled` writes and full-history replays happen while the lock is held, so one slow disk write stalls every client.
- Each client's outbound channel is unbounded. A subscriber that reads slower than publishers write grows server memory without limit.
- The lock is never held across an `.await`, which keeps the synchronous mutex safe inside Tokio.

## 9. Configuration

Sources, later ones winning: built-in defaults, `config/default.toml`, then environment variables using `_` as the separator (`SERVER_PORT`, `BROKER_MAX_RETRIES`).

| Key | Default | Used by |
|---|---|---|
| `server.host` | `127.0.0.1` | Transport |
| `server.port` | `8080` | Transport |
| `server.jwt_secret` | `default_secret` in code, `a_very_secret_key` in `config/default.toml` | Transport |
| `server.jwt_expiration_hours` | `24` | Transport |
| `server.username`, `server.password` | `admin`, `password` | Transport |
| `broker.max_connections` | `1000` | Nothing. Not enforced. |
| `broker.message_ttl_secs` | `3600` | Nothing. Persistence uses its own default. |
| `broker.ack_timeout_ms` | `5000` | Retry loop |
| `broker.max_retries` | `5` | Retry loop |

## 10. Security

**On `main`**

- One shared username and password from config. Any authenticated client may publish or subscribe to any topic.
- JWTs are HS256 with the configured secret. Expiry is checked once, at `auth`; a long-lived connection outlives its token.
- No TLS in the process. Run it behind a TLS-terminating proxy.
- No rate limits, connection cap or message size limit beyond the WebSocket library's defaults.
- Default credentials and secrets ship in the repo, and the server starts happily with them.

**On `feat/add-auth` (unmerged, about 5,000 lines)**

- A user store, role-based permissions per topic, JWT handling in its own crate, and login, sliding-window and token-bucket rate limiters. This covers most of the gaps above and needs a review and merge.

## 11. Failure modes

| Event | What happens |
|---|---|
| Subscriber socket drops | Send loop ends, client and subscriptions are removed. Pending QoS 1 messages for it are lost. |
| Subscriber reads slowly | Its channel grows without bound until memory runs out. |
| Publisher sends to a topic nobody subscribes to | Stored for replay. QoS 1 entry lingers until retries are exhausted. |
| Two publishers reuse a `message_id` | The second overwrites the first in `pending_acks`. |
| `sled` write fails | Logged. The message is still delivered live. |
| Broker process restarts | Retained history survives. In-flight QoS 1 state and all sessions are lost. |
| A task panics while holding the lock | The mutex is poisoned and every later `.lock().expect(...)` panics. The server effectively stops. |

## 12. Known gaps, in priority order

1. ACK state is per message instead of per subscriber.
2. No durable sessions or stable client identity.
3. No resume from an offset. Replay resends all history with new ids.
4. Unbounded per-client queues.
5. One global lock, with disk I/O inside it.
6. Publisher-chosen ids can collide.
7. Delivered messages lack a `type` tag.
8. `max_connections` and `message_ttl_secs` are ignored, and the storage path is hard-coded.
9. Two versions of the `config` crate in the workspace (`popsub_utils` uses 0.13, `popsub_config` uses 0.15), so `PopSubError::Config` can't wrap errors from the loader.
10. `popsub_broker` depends on `tempfile` at runtime because `Broker::default` branches on `cfg!(test)`.
11. Security gaps in section 10, mostly addressed by `feat/add-auth`.
12. No client libraries, release artefacts, health check or metrics.

## 13. Proposed direction

### Phase 1: make the guarantees true

- **Per-subscriber delivery state.** Key pending entries by `(subscriber, message_id)`. An ACK clears only its own entry, and a retry goes only to the subscriber that hasn't acknowledged.
- **Sessions.** The client sends a stable `client_id` at `auth`. Subscriptions and pending deliveries survive a disconnect for a configurable expiry, and are restored on reconnect.
- **Offsets.** Give each stored message a per-topic sequence number and keep its id and QoS. Subscribers resume from their last acknowledged offset instead of replaying everything.
- **Server-owned ids.** The broker always assigns the id. A client-supplied id becomes an idempotency key scoped to that publisher.
- **Backpressure.** Bounded per-client queues. When one fills, disconnect the client (QoS 0) or stop reading from publishers to that topic (QoS 1), and report it in metrics.
- **Concurrency.** Move from one lock to a task per topic that owns its subscribers and pending state, with persistence writes handed to a dedicated writer. The connection task sends commands over channels and never touches shared state directly.
- **Proof.** Failure tests that kill subscribers, restart the broker and slow the disk, asserting the guarantees in section 6 after each change.

### Phase 2: safe to expose

- Review and merge `feat/add-auth`.
- Refuse to start with default secrets or credentials outside a `--dev` flag.
- Enforce connection, message size and rate limits.
- Re-check token expiry on each request, or close connections when tokens expire.
- Document running behind a TLS proxy.

### Phase 3: adoptable

- Wrap deliveries in `ServerMessage::Message` and publish a versioned protocol spec.
- JavaScript and Python client libraries with reconnection and ACK handling built in.
- Release binaries, a Docker image and crates.io packages.
- `/healthz` and Prometheus metrics: connections, queue depths, pending ACKs, retries and drops.
- Published benchmarks against NATS and Redis pub/sub, including where PopSub loses.

### Later: clustering

Replicate topic logs across three nodes with Raft (for example `openraft`). Only worth starting once phase 1 has made the single-node guarantees precise, because replication multiplies every ambiguity above.

## 14. Testing

- 33 tests on `main` across the crates: topic and broker unit tests, persistence round trips, config loading, and WebSocket tests that start a real server on a free port and drive login, auth and errors.
- CI runs `cargo fmt --check`, `cargo clippy -D warnings`, a build and the tests on every push and pull request to `main`. `make ci` runs the same locally.
- Missing: tests for QoS 1 with several subscribers, reconnection, restart recovery and slow consumers. These are the tests phase 1 should start with, written first and failing.

## 15. Why these choices

- **WebSocket and JSON.** Works from browsers and every language without a custom client, at the cost of framing overhead and string-only payloads.
- **One process and `sled`.** No external services to run, which suits demos, edge boxes and local development. The cost is no replication.
- **A synchronous broker behind a lock.** The simplest correct starting point and easy to test without a runtime. Section 13 replaces it once contention matters.
- **Why not Redis pub/sub or NATS?** For production use, one of those is usually the right answer. PopSub exists to understand the problem by building it, and to stay small enough to explain end to end.

## 16. Open questions

- Should QoS 1 sessions expire by time, by count of pending messages, or both?
- Is a per-topic task model enough, or do hot topics need sharding by subscriber?
- Keep `sled`, or move to a plain append-only log file per topic, which would also make offsets and Raft simpler later?
- Should payloads become bytes, with JSON as just one encoding?
