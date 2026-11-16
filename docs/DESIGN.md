# PopSub design

| | |
|---|---|
| Status | Describes `main` as of November 2026, after authentication and rate limiting landed (#9, #12), plus the proposed direction |
| Owner | Soumyadeep Ghosh |
| Scope | The broker, its WebSocket protocol, persistence and delivery guarantees |

## 1. Summary

PopSub is a single-process publish/subscribe broker written in Rust. Clients connect over WebSocket, log in as a user with per-topic permissions, and then subscribe to topics or publish to them using a small JSON protocol. Logins, registrations and publishing are rate limited. Messages can be sent fire-and-forget (QoS 0) or with acknowledgements and retries (QoS 1). Every message is appended to a per-topic log with a sequence number, kept in a directory on disk by default, so subscribers can replay history and durable sessions resume where they left off after a reconnect or a restart.

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
 popsub_connection (per-connection record)   popsub_config (settings loader)
        |                                        |
 popsub_utils (errors, logging)          popsub_storage (topic logs, Storage traits, backends)

 popsub_protocol (every frame type; used by transport, broker, binary and client SDKs)
```

| Crate | Responsibility |
|---|---|
| `popsub` | Binary. Parses the CLI, loads config, refuses published default secrets on public addresses, opens the configured storage backend, starts the broker, the retry and retention loops and the WebSocket server, and exits with a failure status when any of that fails. Also contains a small demo client. |
| `popsub_transport` | `Server` (shared state and the accept loop) and `Session` (one connection, one method per message type). Parses `ClientMessage` JSON, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls. |
| `popsub_auth` | Users and roles, Argon2id password hashing, JWT issue and validation, topic permission patterns. Users are kept in `users.json` in the storage directory with the `file` backend (`FileUserStore`), in memory with the `memory` backend, or in Postgres behind a feature flag. |
| `popsub_ratelimit` | Token bucket, sliding window and login lockout limiters, keyed by string, with pruning of idle keys. |
| `popsub_broker` | One writer task per topic that appends to the log, fans records out in `seq` order and replays history to subscribers; the synchronous `Broker` with subscriber sets and the QoS 1 pending table; all shared through `SharedBroker`. |
| `popsub_sdk` | The Rust client: login and token refresh, reconnection with backoff guided by close codes, resubscription without gaps, confirmed publishes resent under the same id, acknowledgements and deduplication by `(topic, seq)`. The CLI's demo client uses it. |
| `popsub_protocol` | Every frame clients and the server exchange, the error codes and `PROTOCOL_VERSION`. Depends only on `serde`, so client SDKs can use it. |
| `popsub_storage` | Per-topic logs with sequence numbers, subscription cursors, the `Storage` traits, the `file` and `memory` backends and the conformance suite every backend must pass (`docs/STORAGE.md`). |
| `popsub_connection` | `Client`, the server's record of one connected socket (id, `Outbox`, authenticated flag, username, durable session), and `Outbox`, its outgoing queue with a byte budget. |
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

All frames are JSON text, tagged with `type`. The types live in `popsub_protocol`, which the server and client SDKs share. This is protocol version 1 (`PROTOCOL_VERSION`). A JSON Schema generated from those types is in `docs/protocol/` for SDKs in other languages; CI fails if it falls out of date.

- **Request ids.** Any client frame may carry an `id`, a number or a string. The reply carries the same `id`. Frames the server sends on its own have none.
- **Errors.** Every failure is `{"type":"error","id":...,"code":"...","message":"...","retry_after":...}`. `code` is one of `bad_request`, `unauthenticated`, `auth_failed`, `invalid_credentials`, `locked_out`, `rate_limited`, `not_authorized`, `registration_failed`, `invalid_session_id`, `session_unavailable`, `unsupported_protocol`, `storage_unavailable`, `payload_too_large`, `invalid_topic`, `forbidden`, `user_not_found`, `user_exists` or `internal`. `retry_after` is set for `locked_out` and `rate_limited`.
- **Evolution.** Within a version, frames, fields and error codes are only added. Clients ignore fields and frame types they don't know, and treat an unknown error code like `internal`. Removing or changing anything needs a new version, named by the client in `auth`.

Topic names in `subscribe` and `publish` are 1 to 120 bytes (the `file` backend's limit, `popsub_storage::MAX_TOPIC_BYTES`) with no control characters and no `+` or `#`, which are kept for wildcard permission patterns. Anything else gets `invalid_topic`.

### 4.1 Client to server

| `type` | Fields | Reply |
|---|---|---|
| `register` | `username`, `password` | `registered`. Only when `server.allow_registration` is on. Usernames are 3 to 32 letters, digits, `_`, `.` or `-`; passwords at least 8 characters. Allowed before `auth`. |
| `login` | `username`, `password` | `logged_in` with a JWT. Checked against the user store (Argon2id). Rate limited, see section 10. |
| `auth` | `token`, optional `protocol`, optional `session_id`, optional `clean_start` | `authenticated`. Must succeed before anything below is accepted. A bad token, an unsupported `protocol` or a bad `session_id` closes the connection. A `session_id` (1 to 128 characters, no control characters) makes the connection hold that durable session of the token's user; see section 5.4. |
| `subscribe` | `topic`, optional `from` | `subscribed`, then history, then live records. Requires subscribe permission. `from` is `"resume"` (the default: one past a durable session's stored cursor, otherwise no replay), `"new"`, `{"last": N}`, `{"since_ms": T}` or `{"seq": S}`. `storage_unavailable` if the log or session store can't be read, in which case the client is not subscribed. |
| `unsubscribe` | `topic` | `unsubscribed`, even if the client wasn't subscribed. |
| `publish` | `topic`, `payload`, optional `message_id`, optional `qos` (0 or 1) | `published` for QoS 1, nothing for QoS 0. Requires publish permission and is rate limited per user. A repeated `message_id` from the same user is stored and delivered once while the original is retained. If storage fails, either QoS gets `storage_unavailable`. Payloads over `server.max_payload_bytes` get `payload_too_large`, and a `message_id` must be 1 to 128 characters. |
| `ack` | `topic`, `seq` | None. Confirms one QoS 1 record delivered to this connection. Anything not in flight to this connection is ignored. |

The admin requests below need an enabled admin; anyone else gets `forbidden`. A `role` is `{"kind": "admin" | "user" | "readonly", "topics": [{"pattern": "orders/#", "access": "subscribe" | "publish" | "both"}]}`, where `topics` may be left out, is ignored for `admin`, and allows only `subscribe` for `readonly`. Patterns are 1 to 120 bytes with no control characters and no empty levels; `+` must be a whole level and `#` the whole last level. An invalid name, password, pattern or role gets `bad_request`, a missing account `user_not_found`, and a taken name `user_exists`. The last enabled admin can't be disabled, deleted or demoted (`bad_request`), so a server always keeps one.

| `type` | Fields | Reply |
|---|---|---|
| `create_user` | `username`, `password`, `role` | `user`. Same username and password rules as `register`. |
| `get_user` | `username` | `user` |
| `list_users` | none | `users`, sorted by name |
| `set_role` | `username`, `role` | `user`. Replaces the role and its grants. |
| `grant` | `username`, `pattern`, `access` | `user`. Adds the grant, replacing one for exactly that pattern. Refused for admins, and for publishing on a `readonly` account. |
| `revoke` | `username`, `pattern` | `user`. Removes the grant for exactly that pattern; `bad_request` if there is none. |
| `set_password` | `username`, `password` | `user`. Tokens already issued stay valid until they expire. |
| `set_enabled` | `username`, `enabled` | `user` |
| `delete_user` | `username` | `user_deleted` |

Changes take effect on live connections at once. After `set_role`, `grant` or `revoke`, each of the account's subscriptions its role no longer allows is ended: the connection gets `unsubscribed` with no `id`, and a durable session forgets it. Disabling or deleting an account closes its connections with `account_disabled` (4009), and its tokens are refused at `auth` from then on.

### 4.2 Server to client

| Frame | Shape |
|---|---|
| Registered | `{"type":"registered"}` |
| Logged in | `{"type":"logged_in","token":"..."}` |
| Authenticated | `{"type":"authenticated","protocol":1,"limits":{"max_inflight":100,"session_expiry_secs":86400,"heartbeat_secs":30,"idle_timeout_secs":90,"max_payload_bytes":1048576,"max_topic_bytes":120}}`, plus `session_id` and `resumed` (the restored topics) for a durable session |
| Auth expiring | `{"type":"auth_expiring","expires_at_ms":1794800000000}`, a minute before the token expires |
| Subscribed | `{"type":"subscribed","topic":"...","replay_from":41,"replay_to":90}`. Records `replay_from` to `replay_to` are history; later ones are live. Also sent, without an `id`, for each subscription a session restores. |
| Unsubscribed | `{"type":"unsubscribed","topic":"..."}`. Also sent, without an `id`, when an admin change ends a subscription the user may no longer read. |
| User | `{"type":"user","user":{"username":"...","role":{"kind":"user","topics":[...]},"enabled":true,"created_at_ms":1794800000000}}`, the reply to most admin requests. No password hash. |
| Users | `{"type":"users","users":[...]}`, the same objects |
| User deleted | `{"type":"user_deleted","username":"..."}` |
| Published | `{"type":"published","message_id":"...","seq":42}`, for QoS 1 only, once the record is stored |
| Message | `{"type":"message","topic":"...","seq":42,"message_id":"...","payload":"...","qos":0,"published_at_ms":1794800000000}` |
| Truncated | `{"type":"truncated","topic":"...","first_available":1300}`, before records that start later than the subscriber expected |
| Stalled | `{"type":"stalled","topic":"...","seq":42}`, when a record runs out of retries |
| Error | `{"type":"error","code":"not_authorized","message":"..."}`, plus `retry_after` for limits |

All replies also carry the request's `id` when it had one. The broker writes `message`, `truncated`, `stalled` and `subscribed` itself, using the same `popsub_protocol` types as the transport. The publisher's username is stored but not sent to subscribers.

### 4.3 Session order

1. Connect.
2. Optionally `register`. The connection stays open whatever the result.
3. `login`. Wrong credentials or a lockout produce an error and the connection stays open. Malformed frames get `bad_request` and the connection stays open too.
4. `auth` with the token. Failure closes the connection.
5. Any other request. Sending one before successful `auth` produces an error and closes the connection. A permission denial or a publish rate limit produces an error and the connection stays open.
6. Before the token expires, send `auth` again with a new token for the same user to keep the connection.

### 4.4 Connection lifecycle

- **Heartbeat.** The server sends a WebSocket ping every `server.heartbeat_secs` (30). Anything from the client, a pong included, counts as it being alive; after `server.idle_timeout_secs` (90) with nothing, the connection is closed. WebSocket libraries answer pings on their own, so a client only has to keep reading. Both values are in `authenticated.limits`.
- **Token expiry.** The connection remembers its token's `exp`. A minute before, the client gets `{"type":"auth_expiring","expires_at_ms":...}`; at `exp` the connection is closed unless the client has sent `auth` again. A repeat `auth` on an authenticated connection only refreshes the token: it must be for the same user (otherwise `auth_failed`, and the connection keeps its current token), and the reply is `authenticated` with an empty `resumed`.
- **Close codes.** Every close the server starts carries a code and a short reason, so an SDK can decide what to do without parsing text. When the close follows a failed request, an `error` frame comes first. After sending a close, the server waits up to two seconds for the client's close reply before dropping the connection.

| Code | Reason | When | Reconnect? |
|---|---|---|---|
| 1001 | `going_away` | The server is shutting down | Yes, with backoff |
| 1009 | `message_too_big` | A frame was larger than twice `server.max_payload_bytes` plus 64 KiB | Yes, without resending that frame |
| 1011 | `internal_error` | A handler failed unexpectedly | Yes, with backoff |
| 4001 | `auth_failed` | Bad or expired token, or a bad `session_id`, in `auth` | Only after logging in again |
| 4002 | `unauthenticated` | A request before `auth` | Yes, and authenticate first |
| 4003 | `session_taken_over` | Another connection authenticated with the same session | No |
| 4004 | `token_expired` | The token expired without a new `auth` | After logging in again |
| 4005 | `idle_timeout` | Nothing received for the idle timeout | Yes |
| 4006 | `unsupported_protocol` | `auth` named a protocol version the server doesn't speak | No |
| 4007 | `session_unavailable` | The session store couldn't be reached during `auth` | Yes, with backoff |
| 4008 | `slow_consumer` | The client's outgoing queue passed `server.outbound_queue_bytes` | Yes; a durable session resumes from its cursor |
| 4009 | `account_disabled` | An admin disabled or deleted the account, or `auth` presented a token for such an account | No |

The codes are `popsub_protocol::CloseCode`, whose `should_reconnect` encodes the last column.

## 5. Message lifecycle

### 5.1 Publish

1. Transport checks permission and the rate limit, then builds a `NewRecord` with the publisher's username and the client's `message_id` if given.
2. `SharedBroker::publish` sets the publish time, generates an id if missing, and queues the record to the topic's writer.
3. The writer appends it to the topic log at the configured durability, without the broker lock. The log numbers it with the next `seq`, or returns the original record if this publisher already sent this id.
4. If the record is new, the writer takes the broker lock and sends it to every subscription whose next expected `seq` it is and whose window has room. A QoS 1 record goes into each of those subscriptions' windows. Subscriptions that are behind or full are skipped; they are caught up from the log later (5.3). Moved session cursors are then stored, and the log is trimmed to the retention limits.
5. A QoS 1 publisher gets `published` with the `seq` once the append has met the configured durability, including for a retried `message_id`.

### 5.2 Subscribe and replay

1. Transport checks permission. `resume` is turned into one past the session's stored cursor for the topic, or `new` when there is none. The subscribe is queued to the topic's writer.
2. The writer reads the log's bounds and resolves `from` to a starting `seq`. For a durable session it stores the subscription with its cursor just below that start.
3. It adds the subscription with that start, sends `subscribed`, and `truncated` if part of the requested history was trimmed.
4. It catches the subscription up from the log (5.3). Because a subscription is only ever sent the exact `seq` it expects next, live records published later are either sent in order or read back from the log; nothing is skipped or sent twice.

### 5.3 Windows, acknowledgement, retry and catch-up

- Each subscription has its own window of QoS 1 records sent but not acknowledged, at most `broker.max_inflight`. While the window is full, nothing more is sent to it, QoS 0 included, so order is kept.
- `ack` with `topic` and `seq` removes that record from that connection's window only. ACKs for anything else are ignored.
- A subscription's cursor is the highest `seq` with nothing outstanding at or below it: one below its oldest in-flight record, or one below the next record it expects. It moves when records are acknowledged with no gap below them, or when QoS 0 records are sent. A durable session's cursor is written to the subscription store whenever it moves.
- When an ACK frees room and the subscription has missed records, the writer reads them from the log and sends them while the window has room. The same happens when a client's outgoing queue had passed its soft limit and drains again: deliveries skipped meanwhile are read back from the log, so a slow reader is paced instead of queued. Subscribe uses the same catch-up. If records it was due were trimmed meanwhile, it gets `truncated` and moves past them: retention always wins over slow subscribers.
- Every second the retry loop resends records older than `ack_timeout_ms` to the one subscription that hasn't acknowledged them. A record already resent `max_retries` times stalls the subscription instead: it gets `stalled`, and nothing is sent or resent to it until it acknowledges something (which also gives its outstanding records a fresh set of retries) or subscribes again.

### 5.4 Sessions

- `auth` with a `session_id` binds the connection to a durable session keyed by the token's username and that id, so two users with the same id get different sessions.
- Any other connection holding the same session is sent a close frame and removed. This is how a client reconnecting before the server noticed its old connection drop takes the session back.
- `clean_start` discards the stored subscriptions. Otherwise each stored subscription is checked against the user's current permissions (forgotten if no longer allowed), listed in `authenticated.resumed`, and subscribed again with `resume`, so everything after the stored cursor (sent but unacknowledged, or published while away) is replayed.
- Sessions are touched on connect, on disconnect, and every minute while connected. A session not touched for `storage.session_expiry_secs` is removed with its subscriptions.

## 6. Delivery semantics

| | What the code guarantees today |
|---|---|
| QoS 0 | At most once, to subscribers connected at publish time. Retained history is replayed only when the subscriber asks for it with `from`. |
| QoS 1, connected subscribers | At least once per subscriber, independently. Each subscriber's ACK clears only its own copy, and retries go only to subscribers that haven't acknowledged. After `max_retries` resends the subscription stalls and is told, rather than the record being dropped silently. |
| QoS 1, ephemeral subscriber disconnects | Not at least once. Its subscriptions and windows are discarded. |
| QoS 1, durable session disconnects and reconnects | At least once, provided it reconnects within `session_expiry_secs` and the records are still retained. Everything after its stored cursor is replayed. Records trimmed meanwhile are reported with `truncated`. |
| QoS 1, broker restarts | With the `file` backend (the default), durable sessions resume from their stored cursors. Records are kept as firmly as `storage.durability` says: `buffered` survives a crash of the server, `synced` also a power loss. A cursor move is lost only to a power loss, which means resending records already acknowledged. With the `memory` backend nothing survives a restart. |
| Ordering | Per topic, every subscriber receives new records in `seq` order, and a replay followed by live delivery has no gap or repeat. Retries are resent out of order. |
| Duplicates | Possible (retries, and replays of records acknowledged after the cursor was last stored) and detectable: every delivery carries the record's `seq`, so consumers deduplicate by `(topic, seq)`. A publisher retrying with the same `message_id` doesn't create a second record. |
| Retention | History beyond `storage.retention` is trimmed. A subscriber that asks for trimmed history is told with a `truncated` frame. With the `memory` backend, all history is lost on restart. |

The README states these guarantees in short form. Keep the two in step when delivery behaviour changes.

## 7. Persistence

The design is in `docs/STORAGE.md`; rollout steps 1 to 4 and 6 are done, and step 5 (Postgres) is parked.

- Each topic is an append-only log of records in a `popsub_storage::Storage` backend. A record keeps its topic, `seq`, message id, publisher, payload, QoS and publish time.
- `storage.backend` picks the backend: `file` (the default) or `memory`. Any `Storage` implementation can be used by building the server in code.
- The `file` backend keeps each topic in `storage.path/topics/t<topic in hex>/` as segment files of CRC-checked records, plus a session log. It takes an exclusive lock on the directory, so a second server pointed at it refuses to start. On startup it scans every segment, cuts off a half-written record at the end of a topic, and refuses to start if a record anywhere else is damaged. Details in `docs/STORAGE.md` section 7.2.
- `storage.durability` is checked against the backend at startup and refused if the backend can't provide it.
- `storage.retention` limits each topic by record count, age and bytes. Logs are trimmed after each publish and by the retention loop every minute.
- The old `sled` store is gone. Data in an old `pubsub_db` directory was replay history only and is not migrated; delete it with `make reset-data`.

## 8. Concurrency

- Broker state (clients, subscriptions and their windows) is behind one `std::sync::Mutex`, through `SharedBroker::with`. It is held only for short, non-blocking work: fan-out, subscriber changes, ACKs, connects, disconnects and retry passes.
- Storage I/O happens in the topic writers, outside the lock. Each topic's publishes and subscribes are handled one at a time by its writer, so a long replay delays publishes to that topic only.
- Each connection's outgoing queue (`popsub_connection::Outbox`) has a byte budget. Above the soft limit (a sixteenth of `server.outbound_queue_bytes`, at least 64 KiB) no new deliveries are queued: the subscription falls behind and is caught up from the log once the queue drains, the same way as when its QoS 1 window is full. Replies, retries and pings ignore the soft limit; if they push the queue past `server.outbound_queue_bytes`, the connection is closed as a slow consumer.
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
| `server.heartbeat_secs` | `30` | Transport. How often each connection is pinged |
| `server.idle_timeout_secs` | `90` | Transport. How long a connection may send nothing before it is closed |
| `server.outbound_queue_bytes` | `16777216` | Transport. The hard byte limit of each connection's outgoing queue; the soft limit is a sixteenth of it, at least 64 KiB |
| `server.max_payload_bytes` | `1048576` | Transport. The longest publish payload; WebSocket frames over twice this plus 64 KiB are refused before they are buffered |
| `broker.max_connections` | `1000` | Nothing. Not enforced. |
| `broker.ack_timeout_ms` | `5000` | Retry loop |
| `broker.max_retries` | `5` | Retry loop. After this many resends the subscription stalls |
| `broker.max_inflight` | `100` | Window of unacknowledged QoS 1 records per subscription |
| `storage.backend` | `file` | The binary. `file` or `memory` |
| `storage.path` | `pubsub_data` | The `file` backend's directory |
| `storage.durability` | `buffered` | Topic writers. `none`, `buffered` or `synced`; refused at startup if the backend can't provide it |
| `storage.session_expiry_secs` | `86400` | Session expiry loop. How long a durable session is kept after its last connection |
| `storage.retention.max_messages` | `1000` | Trimming |
| `storage.retention.max_age_secs` | `3600` | Trimming |
| `storage.retention.max_bytes` | `67108864` | Trimming. Counts message ids, publishers and payloads |

Rate limits are set in code (`RateLimiters::default`), not in config.

## 10. Security

**Identity and permissions**

- Users live in a `UserStore`. With the `file` storage backend (the default) that is `users.json` in the storage directory: rewritten atomically on every change, readable by its owner only, and kept across restarts. With the `memory` backend accounts are lost on restart. A Postgres store exists behind the `postgres` feature. The admin from `server.username` and `server.password` is added only if no account with that name exists, so once it does, the settings no longer change its password.
- The admin from `server.username` and `server.password` is created at startup with full access.
- Roles: `Admin` (everything), `User` with a list of topic patterns that each allow subscribe, publish or both, and `ReadOnly` with subscribe-only patterns. Patterns use `+` for one level and `#` for the rest. `*` is not special.
- Permissions are looked up from the store on every subscribe and publish, so disabling a user or changing their role takes effect immediately, without waiting for the token to expire. An admin change also ends the account's live subscriptions it no longer allows, and disabling or deleting an account closes its connections and refuses its tokens at `auth`.
- Admins manage accounts with the admin requests (section 4.1) or `popsub users`, which sends them. The last enabled admin can't be disabled, deleted or demoted.
- Self-registration is off by default. When on, new users get the role named by `server.registration_role`, which defaults to no topics, until an admin grants them some.

**Tokens**

- HS256 JWTs signed with `server.jwt_secret`, carrying the username, expiry and role. Refreshing keeps the role, and refuses users who have since been removed or disabled.
- Logging the auth service or its config never prints the secret or the admin password.
- Expiry is enforced on each connection: it is warned a minute before and closed at `exp` unless it sends a fresh `auth` (section 4.4).
- Changing a password doesn't revoke tokens already issued; they stay valid until they expire. Disable the account to cut it off at once.

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
| Subscriber socket drops | Send loop ends, client and subscriptions are removed. An ephemeral connection's unacknowledged records are lost; a durable session gets them again when it reconnects. |
| A connection goes half-open (the client vanished without closing) | No pongs come back, and it is closed with `idle_timeout` after `server.idle_timeout_secs`, freeing its subscriptions and session. |
| A token expires on a long connection | The client is warned with `auth_expiring`, then closed with `token_expired` unless it sent a fresh `auth`. |
| The server is stopped with Ctrl+C | Every client is sent `going_away` before the process exits. |
| Subscriber reads slowly | Once its queue passes the soft limit, deliveries wait in the topic log and are sent as it catches up, in order. Server memory for it stays near the soft limit. If it falls behind retention, it gets `truncated`. |
| Publisher sends to a topic nobody subscribes to | Stored in the topic log until retention trims it. |
| A subscriber stops acknowledging | Its window fills and delivery to it pauses; after `max_retries` it gets `stalled`. Other subscribers are unaffected, and nothing is queued in memory for it. |
| A client acknowledges a record it wasn't sent | Ignored. |
| A publisher retries a message after a dropped connection | Same user and `message_id`: stored and delivered once, while the original is retained. |
| Two publishers reuse a `message_id` | Two separate records; the idempotency key is scoped to the publisher. |
| Someone guesses a user's password | That address and username pair locks out after 5 failures; the address locks out after 20 across all usernames. The real user, from another address, is unaffected. |
| A client floods publishes | Its user's token bucket empties and further publishes get an error until it refills. |
| A client sends a huge frame | Frames over twice `server.max_payload_bytes` plus 64 KiB are refused by the WebSocket layer before they are buffered, and the connection is closed with `message_too_big`. Smaller frames whose payload is over the limit get `payload_too_large`. |
| An admin revokes a user's access while it is subscribed | The subscription is ended with an `unsubscribed` frame, and a durable session forgets it. |
| An admin disables or deletes an account that is connected | Its connections are closed with `account_disabled`; it can't log in, and its tokens are refused at `auth`. |
| The server restarts | With the `file` backend, accounts are read back from `users.json`. With `memory`, every account but the configured admin is lost. |
| Storage append fails | Nothing is delivered. A QoS 1 publisher gets `storage unavailable` and can retry; QoS 0 is logged and dropped. |
| Storage read fails during a subscribe | The client gets an error and is not subscribed. It can subscribe again. |
| A subscriber asks for history that was trimmed | It gets a `truncated` frame with the oldest available `seq`, then the rest. |
| Broker process restarts | With the `file` backend, history and sessions are reopened and sessions resume from their stored cursors. With `memory`, all of it is lost. |
| The server is killed mid-write | On the next start, the half-written record at the end of the topic's newest segment is cut off. Every publish that was confirmed is still there. |
| A stored record is damaged anywhere else | The server refuses to start and names the file and offset, rather than serving data that failed its checksum. |
| Disk full | Appends fail: QoS 1 publishers get `storage unavailable`, nothing is delivered, and the server keeps running. |
| A cursor can't be stored | Logged. The cursor in memory still moves; after a reconnect the session may be sent records it already acknowledged. |
| A task panics while holding the broker lock | The mutex is poisoned. The next `SharedBroker::with` logs it and carries on with the broker's state, so other connections keep working. |
| `accept` fails, for example out of file descriptors | Logged, and the accept loop retries after 100 ms. |
| The port is taken, another server holds `storage.path`, or `storage.durability` is stronger than the backend provides | The binary logs the reason and exits with a failure status. |

## 12. Known gaps, in priority order

1. `max_connections` is ignored, and rate limits aren't configurable.
2. No release artefacts, health check or metrics, and only a Rust client library.
3. Changing a password doesn't revoke the account's outstanding tokens.

## 13. Proposed direction

### Phase 1: make the guarantees true

The storage proposal in `docs/STORAGE.md` covers most of this phase: per-topic logs with sequence numbers, durable per-session cursors, resume, per-subscriber acknowledgement and pluggable backends.

- Done: per-subscriber delivery state, durable sessions, per-topic sequence numbers with resume from a stored cursor, and publisher-scoped idempotency keys (steps 1 to 3 of `docs/STORAGE.md`).
- Done: backpressure. Slow subscribers are paced from the log instead of queued in memory, and only a client that overflows on replies alone is disconnected.
- **Concurrency.** Storage I/O already runs in one writer task per topic, outside the broker lock. Moving subscriber state into those tasks too would remove the remaining shared lock.
- **Proof.** Failure tests that kill subscribers, restart the broker and slow the disk, asserting the guarantees in section 6 after each change.

### Phase 2: safe to expose

Done in #9 and #12: users and per-topic permissions, rate limits on logins, registration and publishing, registration off by default, and refusal of published defaults on public addresses.

Done: admin requests and `popsub users` to create accounts, grant and revoke topics, and disable or delete accounts, with changes applied to live connections.

Remaining:

- Enforce a connection limit, and make rate limits configurable.
- Trust a proxy's forwarded address when configured, so limits work behind one.
- Document running behind a TLS proxy.

### Phase 3: adoptable

- Done:
  - a versioned protocol in `popsub_protocol`, with request ids and error codes
  - close codes, heartbeats, token expiry and re-auth on the same connection
  - payload, frame, topic and message id limits
  - a byte budget for each connection's outbound queue, with slow subscribers caught up from the log
  - a JSON Schema in `docs/protocol/`, generated from the types and checked in CI
- Done: the Rust client library, `popsub_sdk`. Next: TypeScript (types generated from `docs/protocol/`), then Python, held to one shared scenario suite against a real server.
- `wss://` support in `popsub_sdk` (TLS through `tokio-tungstenite`).
- Release binaries, a Docker image and crates.io packages.
- `/healthz` and Prometheus metrics: connections, queue depths, pending ACKs, retries and drops.
- Published benchmarks against NATS and Redis pub/sub, including where PopSub loses.

### Later: clustering

Replicate topic logs across three nodes with Raft (for example `openraft`). Only worth starting once phase 1 has made the single-node guarantees precise, because replication multiplies every ambiguity above.

## 14. Testing

- Every crate has unit tests; `make test` runs them all. The ones that pin down delivery:
  - `popsub_storage` runs its 21-check conformance suite against `MemoryStorage`.
  - `popsub_broker` checks seq order with eight concurrent publishers, that a subscriber joining with full replay while records are published gets every record exactly once, `truncated` frames, publisher retries delivered once, and retention. For QoS 1 it checks that ACKs and retries are per subscriber, that ACKs from other clients are ignored, stalling after `max_retries`, the window holding records back and catch-up from the log after ACKs, cursors only moving over contiguous ACKs, and a session resuming from its cursor after a reconnect and after the broker is rebuilt on the same storage, plus session takeover, clean start and expiry.
  - `popsub_transport` checks the exact JSON of every frame, that the broker's frames parse as `ServerMessage`, and drives a real server through publish confirmations, `from` replay, truncation, and a durable session that reconnects and gets back what it hadn't acknowledged.
- CI runs `cargo fmt --check`, `cargo clippy -D warnings`, a build and the tests on every push and pull request to `main`. `make ci` runs the same locally.
  - The `file` backend runs the same conformance suite with 256-byte segments, plus tests for torn writes, damaged segments, trimming across reopens, the directory lock and the session log. A crash test runs a child process that appends with `synced` durability, kills it at random points over four rounds, and checks every confirmed record is readable. A WebSocket test stops a whole server and starts a new one on the same directory, and a durable session resumes from its cursor.
- Missing: slow consumers that never read their socket, and power-loss testing (killing a process doesn't lose what the operating system has buffered).

## 15. Why these choices

- **WebSocket and JSON.** Works from browsers and every language without a custom client, at the cost of framing overhead and string-only payloads.
- **One process and a directory of log files.** No external services to run, which suits demos, edge boxes and local development. The cost is no replication.
- **A synchronous broker behind a lock.** The simplest correct starting point and easy to test without a runtime. Section 13 replaces it once contention matters.
- **Why not Redis pub/sub or NATS?** For production use, one of those is usually the right answer. PopSub exists to understand the problem by building it, and to stay small enough to explain end to end.

## 16. Open questions

- Is a per-topic task model enough, or do hot topics need sharding by subscriber?
- Should payloads become bytes, with JSON as just one encoding?
