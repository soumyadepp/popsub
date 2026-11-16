# PopSub: a small WebSocket pub/sub server in Rust

PopSub is a small publish/subscribe server written in Rust, meant to be easy to run locally and to read end to end. It supports WebSocket clients, topic-based subscribe/publish, a small JSON protocol, per-topic logs with sequence numbers that subscribers can replay from, durable sessions that resume where they left off, and a QoS 1 option with per-subscriber acknowledgements and retries (at-least-once per subscriber while connected, and across reconnects for durable sessions; see `docs/DESIGN.md` section 6).

Why this exists

- Quick to run locally for demos or simple integrations
- Clear, minimal broker implementation you can read and modify
- Decent starting point for experimenting with WebSockets, message replay, and simple reliability semantics

What's in the workspace

- `crates/popsub`: the CLI (subcommands `server` and `client`)
- `crates/popsub_broker`: broker logic (topic writers, routing, replay, retries)
- `crates/popsub_transport`: WebSocket protocol handling, and enforcing auth, permissions and rate limits
- `crates/popsub_auth`: users, roles, password hashing, JWTs and topic permissions
- `crates/popsub_ratelimit`: token bucket, sliding window and login lockout limiters
- `crates/popsub_storage`: topic logs, the storage traits, the file and in-memory backends and the conformance suite
- `crates/popsub_client`: the server-side record of one connection (not a client library)
- `crates/popsub_config`: config loader and settings
- `crates/popsub_utils`: shared error type and logging

Quick start (five minutes)

1. Build the workspace:

   ```bash
   git clone https://github.com/yourusername/popsub.git
   cd popsub
   cargo build --workspace
   ```

2. Start the server (from the workspace root):

   ```bash
   cargo run -p popsub -- server
   ```

3. Run the client in another terminal:

   ```bash
   cargo run -p popsub -- client --url ws://127.0.0.1:8080
   ```

The client logs in (to get a JWT), authenticates, subscribes to `chat`, publishes a message and prints it when it arrives. Pass `--username`, `--password` and `--topic` to use another account or topic.

Configuration

- The loader reads `config/default.toml` if present, then environment variables, which win. Single-word keys use one underscore after the section (`SERVER_HOST`, `SERVER_PORT`, `SERVER_PASSWORD`). Keys that contain an underscore need two (`SERVER__JWT_SECRET`, `SERVER__ALLOW_REGISTRATION`, `BROKER__MAX_RETRIES`), and so do nested sections (`STORAGE__RETENTION__MAX_MESSAGES`).
- Example `config/default.toml`:

  ```toml
  [server]
  host = "127.0.0.1"
  port = 8080
  jwt_secret = "a_very_secret_key"

  [broker]
  ack_timeout_ms = 5000
  max_retries = 5
  max_inflight = 100

  [storage]
  backend = "file"
  path = "pubsub_data"
  durability = "buffered"
  session_expiry_secs = 86400

  [storage.retention]
  max_messages = 1000
  max_age_secs = 3600
  max_bytes = 67108864
  ```

- `backend = "file"` (the default) keeps topic history and durable sessions in the `path` directory, so they survive restarts. Only one server can use a directory at a time. `backend = "memory"` keeps everything in the process and loses it when the server stops.
- `durability` is how firmly each publish is stored before it is confirmed: `buffered` (the default) survives the server crashing, `synced` also survives a power cut, at the cost of a disk flush per publish. The server refuses to start if the backend can't provide what you ask for.
- `make reset-data` deletes `pubsub_data`.

- The server refuses to listen on anything other than localhost while `jwt_secret` or the admin `password` still have their published default values. On localhost it starts with a warning.
- Registration is off by default. Turn it on with `allow_registration = true` under `[server]`. `registration_role` decides what new users can do: `none` (the default, no topics until an admin grants them), `readonly` or `full`.

Protocol basics (short)

The protocol is versioned (this is version 1) and its types live in the `popsub_protocol` crate. Every frame is a JSON object with a `type`. Any request may carry an `id` (a number or a string); the reply to it carries the same `id`, so a client can match replies to requests. Frames the server sends on its own (messages, `truncated`, `stalled`) have no `id`. Clients should ignore fields and frame types they don't recognise.

Every failure is an `error` frame with a machine-readable `code`:

```json
{ "type": "error", "id": 7, "code": "rate_limited", "message": "too many messages", "retry_after": 1 }
```

The codes are `bad_request`, `unauthenticated`, `auth_failed`, `invalid_credentials`, `locked_out`, `rate_limited`, `not_authorized`, `registration_failed`, `invalid_session_id`, `session_unavailable`, `unsupported_protocol`, `storage_unavailable` and `internal`. A frame that isn't valid JSON or has an unknown `type` gets `bad_request` and the connection stays open.

- Register, when the server allows it:

  ```json
  { "type": "register", "username": "alice", "password": "at-least-8-chars" }
  ```

  The reply is `{ "type": "registered" }`, or an error with code `registration_failed`. Usernames are 3 to 32 letters, digits, `_`, `.` or `-`. Passwords are at least 8 characters. A taken username gets a plain "registration failed", so the endpoint can't be used to discover accounts.

- Login to obtain a JWT:

  ```json
  { "type": "login", "id": 1, "username": "admin", "password": "password" }
  ```

  The reply is `{ "type": "logged_in", "id": 1, "token": "<jwt>" }`, or `invalid_credentials`, or `locked_out` with `retry_after`.

- Use the JWT to authenticate:

  ```json
  { "type": "auth", "token": "<jwt>", "protocol": 1 }
  { "type": "auth", "token": "<jwt>", "protocol": 1, "session_id": "phone-1" }
  ```

  The reply is `{ "type": "authenticated", "protocol": 1, "limits": { "max_inflight": 100, "session_expiry_secs": 86400 } }`. `protocol` may be left out (it means 1); a version the server doesn't speak gets `unsupported_protocol` and the connection closes. With a `session_id`, the connection holds a durable session of that user: its subscriptions and how far it has acknowledged are kept after it disconnects (for `session_expiry_secs`), and restored when it authenticates with the same `session_id` again. The reply then echoes the `session_id` and lists the restored topics in `resumed`. Add `"clean_start": true` to discard what was stored. Reconnecting with a session another connection still holds closes that connection.

- Subscribe / Unsubscribe:

  ```json
  { "type": "subscribe", "topic": "chat" }
  { "type": "subscribe", "topic": "chat", "from": { "last": 50 } }
  { "type": "unsubscribe", "topic": "chat" }
  ```

  The reply to `unsubscribe` is `{ "type": "unsubscribed", "topic": "chat" }`. The reply to `subscribe` is `{ "type": "subscribed", "topic": "chat", "replay_from": 41, "replay_to": 90 }`: messages 41 to 90 are history, anything later is live. `from` decides where replay starts: `"resume"` (the default: continue a durable session's stored subscription, otherwise no history), `"new"` (no history), `{ "last": N }`, `{ "since_ms": <unix ms> }` or `{ "seq": S }`. Replayed and live messages arrive in `seq` order with no gap or repeat between them. If some of the requested history was already trimmed, a `{ "type": "truncated", "topic": "chat", "first_available": 1300 }` frame comes before it. A failure gets an `error` frame instead.

- Publish:

  ```json
  { "type": "publish", "topic": "chat", "payload": "Hello" }

  { "type": "publish", "topic": "critical", "payload": "Important", "message_id": "id-123", "qos": 1 }
  ```

  A QoS 1 publish is confirmed with `{ "type": "published", "message_id": "id-123", "seq": 42 }` once it is stored. A QoS 0 publish gets no reply unless it fails; if storage fails, either QoS gets `storage_unavailable` and nothing was sent. Publishing the same `message_id` again from the same user (for example, retrying after a dropped connection) stores and delivers it once and gets the original `seq` back. When `message_id` is missing the server generates one.

- Messages arrive as:

  ```json
  { "type": "message", "topic": "chat", "seq": 42, "message_id": "id-123", "payload": "Hello", "qos": 1, "published_at_ms": 1794800000000 }
  ```

  `seq` is the message's position in the topic, starting at 1. Use `(topic, seq)` to spot duplicates.

- Acknowledge QoS 1 messages:

  ```json
  { "type": "ack", "topic": "chat", "seq": 42 }
  ```

  Each subscriber acknowledges its own copy; one subscriber's ACK never clears another's. An unacknowledged message is resent after `ack_timeout_ms`, up to `max_retries` times. After that the subscriber gets `{ "type": "stalled", "topic": "chat", "seq": 42 }` and delivery on that topic pauses until it acknowledges something or subscribes again. At most `max_inflight` QoS 1 messages are outstanding per subscription; later ones wait until earlier ones are acknowledged.

Permissions and limits

- Each user has topic permissions. Patterns use MQTT-style wildcards: `+` matches one level and `#` matches the rest. Wildcards apply to permissions only; subscriptions still name an exact topic.
- Failed logins are limited per address and username together (5, then a lockout that grows), and per address across all usernames (20). Registrations are limited to 5 per address per hour. Publishing is limited per user.
- Limits use the address of the connecting socket. Behind a reverse proxy every client shares the proxy's address, so put limits at the proxy too.

Examples and testing

- Run the test suite with `cargo test --workspace`.
- Run Clippy with `cargo clippy --workspace` and format with `cargo fmt`.

Make targets

- `make` lists every target.
- `make demo` starts a server, runs the client against it and stops. Use `make demo PORT=8090` if 8080 is taken.
- `make server`, `make client`, `make test`, `make lint` and `make ci` (the same checks GitHub Actions runs).
- Add `JOBS=2` to any target if a full build runs out of memory.

Design

- `docs/DESIGN.md` covers the protocol, delivery guarantees, persistence, known gaps and the roadmap.

### License: Apache 2.0
