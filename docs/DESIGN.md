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
 popsub_client (per-connection record)   popsub_config (settings loader)
        |                                        |
 popsub_utils (errors, logging)          popsub_storage (topic logs, Storage traits, backends)
```

| Crate | Responsibility |
|---|---|
| `popsub` | Binary. Parses the CLI, loads config, refuses published default secrets on public addresses, opens the configured storage backend, starts the broker, the retry and retention loops and the WebSocket server, and exits with a failure status when any of that fails. Also contains a small demo client. |
| `popsub_transport` | `Server` (shared state and the accept loop) and `Session` (one connection, one method per message type). Parses `ClientMessage` JSON, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls. |
| `popsub_auth` | Users and roles, Argon2id password hashing, JWT issue and validation, topic permission patterns. In-memory user store by default, Postgres behind a feature flag. |
| `popsub_ratelimit` | Token bucket, sliding window and login lockout limiters, keyed by string, with pruning of idle keys. |
| `popsub_broker` | One writer task per topic that appends to the log, fans records out in `seq` order and replays history to subscribers; the synchronous `Broker` with subscriber sets and the QoS 1 pending table; all shared through `SharedBroker`. |
| `popsub_storage` | Per-topic logs with sequence numbers, subscription cursors, the `Storage` traits, an in-memory backend and the conformance suite every backend must pass (`docs/STORAGE.md`). |
| `popsub_persistence` | The old `sled` store. Nothing uses it any more; removed in step 6 of `docs/STORAGE.md`. |
| `popsub_client` | `Client`, the server-side record of one connection: id, outbound channel, authenticated flag and username. Despite the name it is not a client library. |
| `popsub_config` | Loads `Settings` from `config/default.toml` and environment variables, falling back to defaults. Reports published default secrets. |
| `popsub_utils` | `PopSubError`, the shared `Result` alias, and logging setup. |

Dependencies point downwards only. The broker knows nothing about WebSockets, users or permissions; the transport checks those before calling it.

### 3.2 Runtime

```
 socket --> Session (connection task) --publish/subscribe--> topic writer (one task per topic) --append/read--> Storage
            |                                                      |
            |                                                      +--SharedBroker::with--> Broker --> client channel --> send loop --> socket
            |--SharedBroker::with (unsubscribe, ack, auth state)-->                          ^
            |  replies go straight to the session's own channel          retry loop (1s) ---+
            +--> AuthService                                              retention loop (60s) --> Storage
```

- **Server.** Holds what every connection shares (broker, auth service, rate limiters) and runs the accept loop. A failed `accept` is logged and retried after 100 ms instead of stopping the server.
- **Session.** One per socket, in its own task. Reads frames, parses JSON, and calls the method for the message type. Replies are queued on the session's own outgoing channel, so replying never takes the broker lock.
- **Send loop.** One per socket. Drains the client's channel and writes frames. Whichever of the session and the send loop ends first removes the client from the broker.
- **Topic writer.** One task per topic, started on first use. Every publish and subscribe for the topic is queued to it and handled one at a time: it appends to the topic log in storage without holding the broker lock, then takes the lock briefly to fan the record out. Subscribes replay history from the log and then add the subscriber, with no publish handled in between, so replay and live delivery never overlap or leave a gap.
- **Broker.** A `SharedBroker`: the broker behind one `std::sync::Mutex`, reachable only through `with(|broker| ...)`. A closure can't await, so the lock is never held across an await point. If a task panics while holding it, the next caller logs the poisoning and carries on with the broker's state instead of panicking too.
- **Retry loop.** `SharedBroker::run_retry_loop` wakes every second and runs one `Broker::retry_overdue` pass, which resends overdue QoS 1 messages and drops those out of retries.
- **Retention loop.** `SharedBroker::run_retention_loop` trims every topic log to the retention limits once a minute, so quiet topics still age out. Publishing also trims the topic it appended to.
- **Auth service.** A plain `Arc<AuthService>`. Its methods take `&self` and the user store locks internally, so it needs no lock of its own and can never be held together with the broker lock.
- **Prune task.** Every minute, drops rate limiter entries for keys that have gone quiet. It stops when the server stops.

## 4. Protocol

All frames are JSON text. Client messages are tagged with `type`.

### 4.1 Client to server

| `type` | Fields | Notes |
|---|---|---|
| `register` | `username`, `password` | Only when `server.allow_registration` is on. Usernames are 3 to 32 letters, digits, `_`, `.` or `-`; passwords at least 8 characters. Allowed before `auth`. |
| `login` | `username`, `password` | Checked against the user store (Argon2id). Returns a JWT. Rate limited, see section 10. |
| `auth` | `token` | Must succeed before anything below is accepted. A bad token closes the connection. |
| `subscribe` | `topic`, optional `from` | Requires subscribe permission for the topic. `from` is `"new"` (the default: no replay), `{"last": N}`, `{"since_ms": T}` or `{"seq": S}`. History is replayed before live delivery starts. No reply on success; an `error` if the history can't be read, in which case the client is not subscribed. |
| `unsubscribe` | `topic` | |
| `publish` | `topic`, `payload`, optional `message_id`, optional `qos` (0 or 1) | Requires publish permission and is rate limited per user. The broker sets the publish time and generates `message_id` when missing. A repeated `message_id` from the same user is stored and delivered once while the original is retained. If storage fails, a QoS 1 publisher gets `storage unavailable`. |
| `ack` | `message_id` | Confirms a QoS 1 delivery. |

### 4.2 Server to client

| Frame | Shape |
|---|---|
| Register result | `{"type":"register_response","success":true,"message":"..."}` |
| Login result | `{"type":"login_response","token":"..."}` |
| Auth result | `{"type":"authenticated"}` |
| Error | `{"type":"error","message":"..."}` |
| Delivered message | `{"type":"message","topic":"...","seq":42,"message_id":"...","payload":"...","qos":0,"published_at_ms":1794800000000}` |
| Trimmed history | `{"type":"truncated","topic":"...","first_available":1300}`, sent before a replay that starts later than asked |

The broker writes the last two itself (`popsub_broker::TopicFrame`). `ServerMessage::Message` and `ServerMessage::Truncated` wrap the same structs, and a test checks that what the broker writes parses as a `ServerMessage`. The publisher's username is stored but not sent to subscribers.

### 4.3 Session order

1. Connect.
2. Optionally `register`. The connection stays open whatever the result.
3. `login`. Wrong credentials or a lockout produce an error and the connection stays open.
4. `auth` with the token. Failure closes the connection.
5. Any other request. Sending one before successful `auth` produces an error and closes the connection. A permission denial or a publish rate limit produces an error and the connection stays open.

## 5. Message lifecycle

### 5.1 Publish

1. Transport checks permission and the rate limit, then builds a `NewRecord` with the publisher's username and the client's `message_id` if given.
2. `SharedBroker::publish` sets the publish time, generates an id if missing, and queues the record to the topic's writer.
3. The writer appends it to the topic log at the configured durability, without the broker lock. The log numbers it with the next `seq`, or returns the original record if this publisher already sent this id.
4. If the record is new, the writer takes the broker lock, inserts it into `pending_acks` when `qos == 1`, and pushes a `message` frame into every subscriber's channel. Then it trims the log to the retention limits.
5. If the topic has no subscribers the record is still stored, and a QoS 1 record still sits in `pending_acks` until the retry loop gives up on it.

### 5.2 Subscribe and replay

1. Transport checks permission and queues the subscribe to the topic's writer.
2. The writer reads the log's bounds and resolves `from` to a starting `seq`. If part of the requested history was trimmed, it sends a `truncated` frame first.
3. It reads the log from there to the current end in pages of 256 and sends each record with its original `seq`. Replayed records are not added to `pending_acks`.
4. It adds the client to the topic's subscriber set. No publish to the topic is handled between steps 2 and 4, so the client's first live record is the one after the last replayed record.

A subscribe without `from` replays nothing.

### 5.3 Acknowledgement and retry

- `ack` removes the entry from `pending_acks`, whichever subscriber sent it.
- Every second the retry loop finds entries older than `ack_timeout_ms`. Entries that have hit `max_retries` are dropped with a warning; the rest are resent to every current subscriber of the topic.

## 6. Delivery semantics

| | What the code guarantees today |
|---|---|
| QoS 0 | At most once, to subscribers connected at publish time. Retained history is replayed only when the subscriber asks for it with `from`. |
| QoS 1, one subscriber that stays connected | At least once, within `max_retries` attempts. |
| QoS 1, several subscribers | Not at least once. The first ACK from any subscriber clears the message for all of them. Retries go to every subscriber, including those that already acknowledged. |
| QoS 1, subscriber disconnects | Not at least once. The client record and its subscriptions are deleted on disconnect, and a reconnect gets a new identity. |
| QoS 1, broker restarts | Not at least once. `pending_acks` lives only in memory. |
| Ordering | Per topic, every subscriber receives new records in `seq` order, and a replay followed by live delivery has no gap or repeat. Retries are resent out of order. |
| Duplicates | Detectable: every delivery, replay and retry carries the record's `seq`, so consumers deduplicate by `(topic, seq)`. A publisher retrying with the same `message_id` doesn't create a second record. Retries still go to every subscriber. |
| Retention | History beyond `storage.retention` is trimmed. A subscriber that asks for trimmed history is told with a `truncated` frame. With the `memory` backend, all history is lost on restart. |

The README and the portfolio both describe PopSub as at-least-once. That is only accurate for the single-subscriber, always-connected case, and should be stated that way until the phase 1 work lands.

## 7. Persistence

The design is in `docs/STORAGE.md`; rollout steps 1 and 2 are done.

- Each topic is an append-only log of records in a `popsub_storage::Storage` backend. A record keeps its topic, `seq`, message id, publisher, payload, QoS and publish time.
- `storage.backend` picks the backend. Only `memory` exists so far, so history doesn't survive a restart; the `file` backend (step 4) will. Any `Storage` implementation can be used by building the server in code.
- `storage.durability` is checked against the backend at startup and refused if the backend can't provide it.
- `storage.retention` limits each topic by record count, age and bytes. Logs are trimmed after each publish and by the retention loop every minute.
- `popsub_persistence` (the old `sled` store) is no longer used by anything and is removed in step 6.

## 8. Concurrency

- Broker state (clients, subscriber sets, `pending_acks`) is behind one `std::sync::Mutex`, through `SharedBroker::with`. It is held only for short, non-blocking work: fan-out, subscriber changes, ACKs, connects, disconnects and retry passes.
- Storage I/O happens in the topic writers, outside the lock. Each topic's publishes and subscribes are handled one at a time by its writer, so a long replay delays publishes to that topic only.
- Each client's outbound channel is unbounded. A subscriber that reads slower than publishers write grows server memory without limit.
- The lock is never held across an `.await`, which keeps the synchronous mutex safe inside Tokio.

## 9. Configuration

Sources, later ones winning: built-in defaults, `config/default.toml`, then environment variables.

Environment variables name a section and a key. Keys made of one word take a single underscore (`SERVER_HOST`, `SERVER_PORT`, `SERVER_PASSWORD`). Keys that contain an underscore need a double underscore after the section (`SERVER__JWT_SECRET`, `SERVER__ALLOW_REGISTRATION`, `BROKER__MAX_RETRIES`), because a single underscore can't tell the section from the key. Nested sections use a double underscore at every level (`STORAGE__RETENTION__MAX_AGE_SECS`).

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
| `broker.ack_timeout_ms` | `5000` | Retry loop |
| `broker.max_retries` | `5` | Retry loop |
| `storage.backend` | `memory` | The binary. Only `memory` so far |
| `storage.durability` | `none` | Topic writers. `none`, `buffered` or `synced`; refused at startup if the backend can't provide it |
| `storage.retention.max_messages` | `1000` | Trimming |
| `storage.retention.max_age_secs` | `3600` | Trimming |
| `storage.retention.max_bytes` | `67108864` | Trimming. Counts message ids, publishers and payloads |

Rate limits are set in code (`RateLimiters::default`), not in config.

## 10. Security

**Identity and permissions**

- Users live in a `UserStore`: in memory by default (rebuilt at each start, so self-registered users disappear on restart), or Postgres with the `postgres` feature.
- The admin from `server.username` and `server.password` is created at startup with full access.
- Roles: `Admin` (everything), `User` with a list of topic patterns that each allow subscribe, publish or both, and `ReadOnly` with subscribe-only patterns. Patterns use `+` for one level and `#` for the rest. `*` is not special.
- Permissions are looked up from the store on every subscribe and publish, so disabling a user or changing their role takes effect immediately, without waiting for the token to expire.
- Self-registration is off by default. When on, new users get the role named by `server.registration_role`, which defaults to no topics.

**Tokens**

- HS256 JWTs signed with `server.jwt_secret`, carrying the username, expiry and role. Refreshing keeps the role, and refuses users who have since been removed or disabled.
- Logging the auth service or its config never prints the secret or the admin password.
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
| Publisher sends to a topic nobody subscribes to | Stored in the topic log. QoS 1 entry lingers until retries are exhausted. |
| A publisher retries a message after a dropped connection | Same user and `message_id`: stored and delivered once, while the original is retained. |
| Two publishers reuse a `message_id` | The second overwrites the first in `pending_acks`. |
| Someone guesses a user's password | That address and username pair locks out after 5 failures; the address locks out after 20 across all usernames. The real user, from another address, is unaffected. |
| A client floods publishes | Its user's token bucket empties and further publishes get an error until it refills. |
| The server restarts | Self-registered users are lost with the in-memory store. The admin is recreated from config. |
| Storage append fails | Nothing is delivered. A QoS 1 publisher gets `storage unavailable` and can retry; QoS 0 is logged and dropped. |
| Storage read fails during a subscribe | The client gets an error and is not subscribed. It can subscribe again. |
| A subscriber asks for history that was trimmed | It gets a `truncated` frame with the oldest available `seq`, then the rest. |
| Broker process restarts | With the `memory` backend, all history is lost. In-flight QoS 1 state and all sessions are lost. |
| A task panics while holding the broker lock | The mutex is poisoned. The next `SharedBroker::with` logs it and carries on with the broker's state, so other connections keep working. |
| `accept` fails, for example out of file descriptors | Logged, and the accept loop retries after 100 ms. |
| The port is taken, or `storage.durability` is stronger than the backend provides | The binary logs the reason and exits with a failure status. |

## 12. Known gaps, in priority order

1. ACK state is per message instead of per subscriber.
2. No durable sessions or stable client identity.
3. No resume from a stored cursor. Subscribers can replay from a `seq` or a time, but the server doesn't remember where they were (step 3 of `docs/STORAGE.md`).
4. Unbounded per-client queues.
5. `pending_acks` is keyed by message id alone, so two publishers using the same id overwrite each other's pending entry.
6. No durable storage backend yet: the only one, `memory`, loses history on restart (step 4).
7. No way to grant topics over the protocol. A self-registered user with the default `none` role can't do anything until someone changes their role through the `AuthService` API in code.
8. Token expiry is only checked at `auth`.
9. `max_connections` is ignored, and rate limits aren't configurable.
10. No client libraries, release artefacts, health check or metrics.

## 13. Proposed direction

### Phase 1: make the guarantees true

The storage proposal in `docs/STORAGE.md` covers most of this phase: per-topic logs with sequence numbers, durable per-session cursors, resume, per-subscriber acknowledgement and pluggable backends.

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

- Publish a versioned protocol spec.
- JavaScript and Python client libraries with reconnection and ACK handling built in.
- Release binaries, a Docker image and crates.io packages.
- `/healthz` and Prometheus metrics: connections, queue depths, pending ACKs, retries and drops.
- Published benchmarks against NATS and Redis pub/sub, including where PopSub loses.

### Later: clustering

Replicate topic logs across three nodes with Raft (for example `openraft`). Only worth starting once phase 1 has made the single-node guarantees precise, because replication multiplies every ambiguity above.

## 14. Testing

- Every crate has unit tests; `make test` runs them all. The ones that pin down delivery:
  - `popsub_storage` runs its 21-check conformance suite against `MemoryStorage`.
  - `popsub_broker` checks seq order with eight concurrent publishers, that a subscriber joining with full replay while records are published gets every record exactly once, `truncated` frames, publisher retries delivered once, and retention on publish and in the background loop. Both concurrency tests fail if publishes and subscribes stop going through one writer per topic.
  - `popsub_transport` checks the exact JSON of every frame, that the broker's frames parse as `ServerMessage`, and drives a real server through publish, `from` replay and truncation.
- CI runs `cargo fmt --check`, `cargo clippy -D warnings`, a build and the tests on every push and pull request to `main`. `make ci` runs the same locally.
- Missing: tests for QoS 1 with several subscribers, reconnection, restart recovery and slow consumers. Step 3 of `docs/STORAGE.md` adds the first two; restart recovery needs the `file` backend.

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
