# PopSub design

| | |
|---|---|
| Status | Describes `main` as of November 2026, after authentication and rate limiting landed (#9, #12), plus the proposed direction |
| Owner | Soumyadeep Ghosh |
| Scope | The broker, its WebSocket protocol, persistence and delivery guarantees |

## 1. Summary

PopSub is a single-process publish/subscribe broker written in Rust. Clients connect over WebSocket, log in as a user with per-topic permissions, and then subscribe to topics or publish to them using a small JSON protocol. Logins, registrations and publishing are rate limited. Messages can be sent fire-and-forget (QoS 0) or with acknowledgements and retries (QoS 1). Recent messages are stored per topic in an embedded `sled` database and replayed to new subscribers.

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
 popsub_transport (WebSocket, protocol, auth and limit enforcement)
        |                 |                    |
 popsub_broker      popsub_auth          popsub_ratelimit
 (topics, QoS 1)    (users, JWT, RBAC)   (token bucket, sliding window, lockouts)
        |
 popsub_persistence   popsub_client (per-connection record)
        |
 popsub_utils (errors, logging)       popsub_config (settings loader)
```

| Crate | Responsibility |
|---|---|
| `popsub` | Binary. Parses the CLI, loads config, refuses published default secrets on public addresses, opens the message store, starts the broker, the retry loop and the WebSocket server, and exits with a failure status when any of that fails. Also contains a small demo client. |
| `popsub_transport` | `Server` (shared state and the accept loop) and `Session` (one connection, one method per message type). Parses `ClientMessage` JSON, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls. |
| `popsub_auth` | Users and roles, Argon2id password hashing, JWT issue and validation, topic permission patterns. In-memory user store by default, Postgres behind a feature flag. |
| `popsub_ratelimit` | Token bucket, sliding window and login lockout limiters, keyed by string, with pruning of idle keys. |
| `popsub_broker` | The engine: topics, subscriber sets, publishing, QoS 1 pending table and retries, replay on subscribe. Synchronous API, shared through `SharedBroker`. |
| `popsub_persistence` | `sled` store of recent messages per topic, with a TTL and a per-topic cap. |
| `popsub_client` | `Client`, the server-side record of one connection: id, outbound channel, authenticated flag and username. Despite the name it is not a client library. |
| `popsub_config` | Loads `Settings` from `config/default.toml` and environment variables, falling back to defaults. Reports published default secrets. |
| `popsub_utils` | `PopSubError`, the shared `Result` alias, and logging setup. |

Dependencies point downwards only. The broker knows nothing about WebSockets, users or permissions; the transport checks those before calling it.

### 3.2 Runtime

```
 socket --> Session (connection task) --SharedBroker::with--> Broker --> client channel (unbounded mpsc) --> send loop --> socket
            |  replies go straight to the session's own channel           ^
            |                                    retry loop (every 1s)    |
            +--> AuthService (RwLock, released before the broker is used)  v
                                                                         sled
```

- **Server.** Holds what every connection shares (broker, auth service, rate limiters) and runs the accept loop. A failed `accept` is logged and retried after 100 ms instead of stopping the server.
- **Session.** One per socket, in its own task. Reads frames, parses JSON, and calls the method for the message type. Replies are queued on the session's own outgoing channel, so replying never takes the broker lock.
- **Send loop.** One per socket. Drains the client's channel and writes frames. Whichever of the session and the send loop ends first removes the client from the broker.
- **Broker.** A `SharedBroker`: the broker behind one `std::sync::Mutex`, reachable only through `with(|broker| ...)`. A closure can't await, so the lock is never held across an await point. If a task panics while holding it, the next caller logs the poisoning and carries on with the broker's state instead of panicking too.
- **Retry loop.** `SharedBroker::run_retry_loop` wakes every second and runs one `Broker::retry_overdue` pass, which resends overdue QoS 1 messages and drops those out of retries.
- **Auth service.** An `Arc<tokio::sync::RwLock<AuthService>>`. Handlers release it before touching the broker, so the two locks are never held together.
- **Prune task.** Every minute, drops rate limiter entries for keys that have gone quiet. It stops when the server stops.

## 4. Protocol

All frames are JSON text. Client messages are tagged with `type`.

### 4.1 Client to server

| `type` | Fields | Notes |
|---|---|---|
| `register` | `username`, `password` | Only when `server.allow_registration` is on. Usernames are 3 to 32 letters, digits, `_`, `.` or `-`; passwords at least 8 characters. Allowed before `auth`. |
| `login` | `username`, `password` | Checked against the user store (Argon2id). Returns a JWT. Rate limited, see section 10. |
| `auth` | `token` | Must succeed before anything below is accepted. A bad token closes the connection. |
| `subscribe` | `topic` | Requires subscribe permission for the topic. Creates the topic if needed and replays stored messages. |
| `unsubscribe` | `topic` | |
| `publish` | `topic`, `payload`, optional `message_id`, optional `qos` (0 or 1) | Requires publish permission and is rate limited per user. Broker sets `timestamp` and generates `message_id` when missing. |
| `ack` | `message_id` | Confirms a QoS 1 delivery. |

### 4.2 Server to client

| Frame | Shape |
|---|---|
| Register result | `{"type":"register_response","success":true,"message":"..."}` |
| Login result | `{"type":"login_response","token":"..."}` |
| Auth result | `{"type":"authenticated"}` |
| Error | `{"type":"error","message":"..."}` |
| Delivered message | `{"topic":"...","payload":"...","timestamp":0,"message_id":"...","qos":0}` |

**Inconsistency to fix:** delivered messages are the broker's `Message` struct serialised directly, so they carry no `type` field. `ServerMessage::Message` exists in the transport crate but is never used. Clients currently have to detect deliveries by the absence of `type`. Section 12 proposes wrapping deliveries in `ServerMessage::Message`.

### 4.3 Session order

1. Connect.
2. Optionally `register`. The connection stays open whatever the result.
3. `login`. Wrong credentials or a lockout produce an error and the connection stays open.
4. `auth` with the token. Failure closes the connection.
5. Any other request. Sending one before successful `auth` produces an error and closes the connection. A permission denial or a publish rate limit produces an error and the connection stays open.

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
- The binary opens the store at `pubsub_db` relative to the working directory, with the TTL from `broker.message_ttl_secs` and a cap of 1000 messages per topic. The path and cap aren't configurable. `sled` locks the directory, so a second server started from the same directory exits with an error instead of sharing it.
- `Broker::new` (used by tests and `Broker::default`) still opens `Persistence::default()`, which panics when the store can't be opened.
- `sled` 0.34 has been the stable line for years while its successor stays in alpha. It is fine for this scale, but it is a dependency risk worth tracking.

## 8. Concurrency

- Every operation takes the same `std::sync::Mutex`, through `SharedBroker::with`. Publishes, subscribes, ACKs, connects, disconnects and the retry loop all queue behind each other.
- `sled` writes and full-history replays happen while the lock is held, so one slow disk write stalls every client.
- Each client's outbound channel is unbounded. A subscriber that reads slower than publishers write grows server memory without limit.
- The lock is never held across an `.await`, which keeps the synchronous mutex safe inside Tokio.

## 9. Configuration

Sources, later ones winning: built-in defaults, `config/default.toml`, then environment variables.

Environment variables name a section and a key. Keys made of one word take a single underscore (`SERVER_HOST`, `SERVER_PORT`, `SERVER_PASSWORD`). Keys that contain an underscore need a double underscore after the section (`SERVER__JWT_SECRET`, `SERVER__ALLOW_REGISTRATION`, `BROKER__MAX_RETRIES`), because a single underscore can't tell the section from the key.

| Key | Default | Used by |
|---|---|---|
| `server.host` | `127.0.0.1` | Transport, and the default-secrets check |
| `server.port` | `8080` | Transport |
| `server.jwt_secret` | `default_secret` in code, `a_very_secret_key` in `config/default.toml` | Auth |
| `server.jwt_expiration_hours` | `24` | Auth |
| `server.username`, `server.password` | `admin`, `password` | Auth, which creates this admin user at startup |
| `server.allow_registration` | `false` | Auth |
| `server.registration_role` | `none` | Auth. `none`, `readonly` or `full`; anything else means `none` |
| `broker.max_connections` | `1000` | Nothing. Not enforced. |
| `broker.message_ttl_secs` | `3600` | Message store TTL |
| `broker.ack_timeout_ms` | `5000` | Retry loop |
| `broker.max_retries` | `5` | Retry loop |

Rate limits are set in code (`RateLimiters::default`), not in config.

## 10. Security

**Identity and permissions**

- Users live in a `UserStore`: in memory by default (rebuilt at each start, so self-registered users disappear on restart), or Postgres with the `postgres` feature.
- The admin from `server.username` and `server.password` is created at startup with full access.
- Roles: `Admin` (everything), `User` with a list of topic patterns that each allow subscribe, publish or both, and `ReadOnly` with subscribe-only patterns. Patterns use `+` for one level and `#` for the rest. `*` is not special.
- Permissions are looked up from the store on every subscribe and publish, so disabling a user or changing their role takes effect immediately, without waiting for the token to expire.
- Self-registration is off by default. When on, new users get the role named by `server.registration_role`, which defaults to no topics.

**Tokens**

- HS256 JWTs signed with `server.jwt_secret`, carrying the username, expiry and role. Refreshing keeps the role.
- Expiry is checked once, at `auth`. A long-lived connection outlives its token.

**Limits**

| What | Key | Limit |
|---|---|---|
| Failed logins | address and username together | 5, then a lockout from 5 minutes that doubles up to an hour |
| Failed logins | address, across all usernames | 20, then a lockout from 15 minutes that doubles up to an hour |
| Registration attempts | address | 5 per hour, successful or not |
| Publishing | username, shared by all of a user's connections | 100 a second, bursts of 50 |

- Keying failed logins by address and username together means someone guessing a password from one machine locks out only themselves.
- Limits use the socket's address. Behind a reverse proxy every client shares the proxy's address, so put limits at the proxy too.
- Idle limiter entries are pruned every minute, so memory doesn't grow with every address or username ever tried.

**Defaults and exposure**

- The server refuses to listen on a non-loopback address while `jwt_secret` is one of the published defaults or the admin password is `password`. On localhost it starts and logs a warning. The auth library also warns when constructed with those values.
- Taken usernames get a generic "registration failed", so registration can't be used to discover accounts.
- No TLS in the process. Run it behind a TLS-terminating proxy.

## 11. Failure modes

| Event | What happens |
|---|---|
| Subscriber socket drops | Send loop ends, client and subscriptions are removed. Pending QoS 1 messages for it are lost. |
| Subscriber reads slowly | Its channel grows without bound until memory runs out. |
| Publisher sends to a topic nobody subscribes to | Stored for replay. QoS 1 entry lingers until retries are exhausted. |
| Two publishers reuse a `message_id` | The second overwrites the first in `pending_acks`. |
| Someone guesses a user's password | That address and username pair locks out after 5 failures; the address locks out after 20 across all usernames. The real user, from another address, is unaffected. |
| A client floods publishes | Its user's token bucket empties and further publishes get an error until it refills. |
| The server restarts | Self-registered users are lost with the in-memory store. The admin is recreated from config. |
| `sled` write fails | Logged. The message is still delivered live. |
| Broker process restarts | Retained history survives. In-flight QoS 1 state and all sessions are lost. |
| A task panics while holding the broker lock | The mutex is poisoned. The next `SharedBroker::with` logs it and carries on with the broker's state, so other connections keep working. |
| `accept` fails, for example out of file descriptors | Logged, and the accept loop retries after 100 ms. |
| The port is taken, or another server holds the store | The binary logs the reason and exits with a failure status. |

## 12. Known gaps, in priority order

1. ACK state is per message instead of per subscriber.
2. No durable sessions or stable client identity.
3. No resume from an offset. Replay resends all history with new ids.
4. Unbounded per-client queues.
5. One global lock, with disk I/O inside it.
6. Publisher-chosen ids can collide.
7. Delivered messages lack a `type` tag.
8. No way to grant topics over the protocol. A self-registered user with the default `none` role can't do anything until someone changes their role through the `AuthService` API in code.
9. Token expiry is only checked at `auth`.
10. `max_connections` is ignored, the storage path and per-topic cap are hard-coded, and rate limits aren't configurable.
11. Two versions of the `config` crate in the workspace (`popsub_utils` uses 0.13, `popsub_config` uses 0.15), so `PopSubError::Config` can't wrap errors from the loader.
12. `popsub_broker` depends on `tempfile` at runtime because `Broker::default` branches on `cfg!(test)`.
13. No client libraries, release artefacts, health check or metrics.

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

Done in #9 and #12: users and per-topic permissions, rate limits on logins, registration and publishing, registration off by default, and refusal of published defaults on public addresses.

Remaining:

- An admin message or CLI command to grant topics and manage users.
- Close connections when their token expires, or re-check expiry on each request.
- Enforce connection and message size limits, and make rate limits configurable.
- Trust a proxy's forwarded address when configured, so limits work behind one.
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

- 97 tests on `main` across the crates: topic and broker unit tests, persistence round trips, config loading including environment variables, auth (registration rules, roles, wildcards, tokens, default secrets), rate limiters (including concurrent resets and pruning), and WebSocket tests that start a real server on a free port and drive registration, login, lockouts, auth and errors.
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
