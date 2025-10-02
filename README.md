# PopSub — a tiny WebSocket pub/sub server in Rust

PopSub is a small, practical Pub/Sub server written in Rust. It’s meant to be simple to run locally and easy to study or extend. It supports WebSocket clients, topic-based subscribe/publish, a small JSON protocol, a replay buffer for recent messages, and a QoS=1 (at-least-once) delivery option with acknowledgements.

Why this exists

- Quick to run locally for demos or simple integrations
- Clear, minimal broker implementation you can read and modify
- Decent starting point for experimenting with WebSockets, message replay, and simple reliability semantics

What’s in the workspace

- `crates/popsub` — the small CLI (subcommands: `server`, `client`)
- `crates/popsub_broker` — in-memory broker logic (topics, routing, retries)
- `crates/popsub_transport` — WebSocket protocol handling
- `crates/popsub_persistence` — optional sled-based persistence
- `crates/popsub_client` — client helpers and example code
- `crates/popsub_config` — config loader and settings
- `crates/popsub_utils` — small utilities (logging, errors)

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

3. Run the example client in another terminal:

   ```bash
   cargo run -p popsub -- client --url ws://127.0.0.1:8080
   ```

The example client connects, logs in (to get a JWT), authenticates, subscribes to `chat`, and publishes a small message.

Configuration

- The loader reads `config/default.toml` if present, and falls back to environment variables. Environment variables use `_` as a separator: `SERVER_HOST`, `SERVER_PORT`, `BROKER_MAX_CONNECTIONS`.
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

Protocol basics (short)

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

Examples and testing

- A simple example client is at `crates/popsub/examples/simple_client.rs`.
- Run the test suite with `cargo test --workspace`.
- Run Clippy with `cargo clippy --workspace` and format with `cargo fmt`.

### License: Apache 2.0
