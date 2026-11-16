# PopSub: a small WebSocket pub/sub server in Rust

PopSub is a small publish/subscribe server written in Rust, meant to be easy to run locally and to read end to end. It supports WebSocket clients, topic-based subscribe/publish, a small JSON protocol, a replay buffer for recent messages, and a QoS 1 option with acknowledgements and retries (at-least-once for a single subscriber that stays connected; see `docs/DESIGN.md` section 6).

Why this exists

- Quick to run locally for demos or simple integrations
- Clear, minimal broker implementation you can read and modify
- Decent starting point for experimenting with WebSockets, message replay, and simple reliability semantics

What's in the workspace

- `crates/popsub`: the CLI (subcommands `server` and `client`)
- `crates/popsub_broker`: in-memory broker logic (topics, routing, retries)
- `crates/popsub_transport`: WebSocket protocol handling, and enforcing auth, permissions and rate limits
- `crates/popsub_auth`: users, roles, password hashing, JWTs and topic permissions
- `crates/popsub_ratelimit`: token bucket, sliding window and login lockout limiters
- `crates/popsub_persistence`: sled storage of recent messages per topic
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

- The loader reads `config/default.toml` if present, then environment variables, which win. Single-word keys use one underscore after the section (`SERVER_HOST`, `SERVER_PORT`, `SERVER_PASSWORD`). Keys that contain an underscore need two (`SERVER__JWT_SECRET`, `SERVER__ALLOW_REGISTRATION`, `BROKER__MAX_RETRIES`).
- Example `config/default.toml`:

  ```toml
  [server]
  host = "127.0.0.1"
  port = 8080
  jwt_secret = "a_very_secret_key"

  [broker]
  max_connections = 1000
  message_ttl_secs = 3600
  ```

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
  { "type": "unsubscribe", "topic": "chat" }
  ```

- Publish:

  ```json
  { "type": "publish", "topic": "chat", "payload": "Hello" }

  { "type": "publish", "topic": "critical", "payload": "Important", "message_id": "id-123", "qos": 1 }
  ```

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
