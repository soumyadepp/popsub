# PopSub — WebSocket Pub/Sub Server in Rust 🦀

**PopSub** is a minimalist, in-memory publish/subscribe server built with Rust, `tokio`, and `tokio-tungstenite`. It supports:

- WebSocket client connections
- Topic-based `subscribe`, `unsubscribe`, and `publish`
- Real-time message broadcasting to all subscribers
- Client management with per-client message channels

---

## 🚀 Features

- Async runtime with Tokio
- Safe sharing of state across threads using `Arc<Mutex<…>>`
- JSON-based messaging using `serde`
- Prevents duplicate subscriptions
- Supports clean client registration & removal

---

## 🛠️ Prerequisites

- Rust (1.70+ recommended) with `cargo`
- `rustfmt` (optional, for formatting)
- `tokio`, `tokio-tungstenite`, `serde`, etc. (configured in `Cargo.toml`)

---

## ⚙️ Setup

1. Clone the repo:
   ```bash
   git clone https://github.com/yourusername/popsub.git
   cd popsub
   ```
2. Install rustfmt (optional but recommended):
   ```bash
   rustup component add rustfmt
   ```
3. Build the project:
   ```bash
   cargo build
   ```
4. Run the formatter:
   ```bash
   cargo fmt
   ```

## ▶️ Run the Server

Start the server with:

```bash
cargo run
```

By default, it reads host and port from config/default.toml or .env. Example:

```toml
server.host = "127.0.0.1"
server.port = 8000
```

You should see:

```bash
WebSocket server listening on ws://127.0.0.1:8000
```

## 💬 Messaging Protocol

Clients communicate using JSON messages:

```json
// Subscribe
{ "type": "subscribe", "topic": "chat" }

// Unsubscribe
{ "type": "unsubscribe", "topic": "chat" }

// Publish
{
  "type": "publish",
  "topic": "chat",
  "payload": "Hello world!",
  "timestamp": 1689475200
}
```

Broadcasted messages look like:

```json
{
  "topic": "chat",
  "payload": "Hello world!",
  "timestamp": 1689475200
}
```

## Testing with WebSocket King

1. Open [WebSocket King](https://websocketking.com/)

2. Connect to:

```bash
127.0.0.1:8080
```

3. In Tab 1, subscribe:

```json
{ "type": "subscribe", "topic": "chat" }
```

4. In Tab 2, subscribe to the same topic.

5. In Tab 1, publish:

```json
{
  "type": "publish",
  "topic": "chat",
  "payload": "Hello from Tab 1!",
  "timestamp": 1689475300
}
```

6. You should see that message appear in Tab 2 — 🎉 real-time pub/sub in action!

## Next Steps

✅ Clean up disconnected clients automatically

💾 Add persistence or message history

🔐 Add authentication and access control

🌐 Build a web UI or API client

## 📜 License

Apache 2.0
