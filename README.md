# PopSub: a small WebSocket pub/sub server in Rust

PopSub is a small publish/subscribe server written in Rust, meant to be easy to run locally and to read end to end. It supports WebSocket clients, topic-based subscribe/publish, a small JSON protocol, per-topic logs with sequence numbers that subscribers can replay from, and a QoS 1 option with acknowledgements and retries (at-least-once for a single subscriber that stays connected; see `docs/DESIGN.md` section 6).

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
- `crates/popsub_storage`: topic logs, the storage traits, the in-memory backend and the conformance suite
- `crates/popsub_persistence`: the old `sled` store, no longer used and due to be removed
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

  [storage]
  backend = "memory"
  durability = "none"

  [storage.retention]
  max_messages = 1000
  max_age_secs = 3600
  max_bytes = 67108864
  ```

- `memory` is the only backend so far, so topic history is lost when the server stops. A file backend that survives restarts is planned (`docs/STORAGE.md`). The server refuses to start if `durability` is stronger than the backend can provide.

- The server refuses to listen on anything other than localhost while `jwt_secret` or the admin `password` still have their published default values. On localhost it starts with a warning.
- Registration is off by default. Turn it on with `allow_registration = true` under `[server]`. `registration_role` decides what new users can do: `none` (the default, no topics until an admin grants them), `readonly` or `full`.

Protocol basics (short)

- Register, when the server allows it:

  ```json
  { "type": "register", "username": "alice", "password": "at-least-8-chars" }
  ```

  The reply is `{ "type": "register_response", "success": true, "message": "..." }`. Usernames are 3 to 32 letters, digits, `_`, `.` or `-`. Passwords are at least 8 characters. A taken username gets a plain "registration failed", so the endpoint can't be used to discover accounts.

- Login to obtain a JWT:

  ```json
  { "type": "login", "username": "admin", "password": "password" }
  ```

- Use the JWT to authenticate:

  ```json
  { "type": "auth", "token": "<jwt>" }
  ```

- Subscribe / Unsubscribe:

  ```json
  { "type": "subscribe", "topic": "chat" }
  { "type": "subscribe", "topic": "chat", "from": { "last": 50 } }
  { "type": "unsubscribe", "topic": "chat" }
  ```

  Without `from`, a subscriber gets only messages published after it subscribes. To replay history first, `from` can be `"new"`, `{ "last": N }`, `{ "since_ms": <unix ms> }` or `{ "seq": S }`. Replayed and live messages arrive in `seq` order with no gap or repeat between them. If some of the requested history was already trimmed, a `{ "type": "truncated", "topic": "chat", "first_available": 1300 }` frame comes first. There is no reply to a successful subscribe; a failure gets an `error` frame.

- Publish:

  ```json
  { "type": "publish", "topic": "chat", "payload": "Hello" }

  { "type": "publish", "topic": "critical", "payload": "Important", "message_id": "id-123", "qos": 1 }
  ```

  Publishing the same `message_id` again from the same user (for example, retrying after a dropped connection) stores and delivers it once. When `message_id` is missing the server generates one.

- Messages arrive as:

  ```json
  { "type": "message", "topic": "chat", "seq": 42, "message_id": "id-123", "payload": "Hello", "qos": 1, "published_at_ms": 1794800000000 }
  ```

  `seq` is the message's position in the topic, starting at 1. Use `(topic, seq)` to spot duplicates.

- Acknowledge QoS=1 messages:

  ```json
  { "type": "ack", "message_id": "id-123" }
  ```

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
