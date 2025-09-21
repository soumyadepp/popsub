# PopSub — WebSocket Pub/Sub Server in Rust 🦀

**PopSub** is a minimalist, in-memory publish/subscribe server built with Rust, `tokio`, and `tokio-tungstenite`. It supports:

- WebSocket-based client communication
- Topic-based `subscribe`, `unsubscribe`, and `publish`
- Real-time message broadcasting
- Per-client message channels
- Replay of past messages to reconnecting clients
- **Quality of Service (QoS) Level 1 (At Least Once) delivery with retry mechanism**
- **Acknowledgement (ACK) mechanism for reliable message delivery**

---

## 🚀 Features

- Asynchronous runtime powered by **Tokio**
- **Safe concurrency** with `Arc<Mutex<...>>` and `DashMap`
- **JSON-based protocol** with `serde`
- Prevents **duplicate subscriptions**
- **Graceful client disconnection handling**
- **Message history** replay support on reconnect
- **QoS Level 1 (At Least Once) delivery with configurable retries**
- **Acknowledgement (ACK) mechanism for reliable message delivery**
- Extensible and minimal design

---

## 🛠️ Prerequisites

- Rust (`1.70+` recommended)
- [`rustfmt`](https://rust-lang.github.io/rustfmt/) (optional for code formatting)
- Dependencies like:
  - `tokio`
  - `tokio-tungstenite`
  - `serde`, `serde_json`
  - `futures`, `dashmap`, etc. (defined in `Cargo.toml`)

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

// Publish (QoS 0 - At Most Once)
{
  "type": "publish",
  "topic": "chat",
  "payload": "Hello world!"
}

// Publish (QoS 1 - At Least Once)
// Requires a unique message_id. Broker will re-send if no ACK is received.
{
  "type": "publish",
  "topic": "my_critical_topic",
  "payload": "Important message!",
  "message_id": "some_unique_message_id_123",
  "qos": 1
}

// Acknowledge a QoS 1 message
// Send this after successfully processing a QoS 1 message.
{
  "type": "ack",
  "message_id": "some_unique_message_id_123"
}
```

Broadcasted messages look like:

```json
{
  "topic": "chat",
  "payload": "Hello world!",
  "timestamp": 1689475200,
  "message_id": "generated_or_provided_id",
  "qos": 0 // or 1
}
```

## 🔁 Reconnect and Replay

PopSub supports automatic **message replay** for clients that disconnect and reconnect.

- When a client reconnects and **resubscribes to a topic**, the server **replays the last N messages** (e.g., last 10) published to that topic.
- This ensures clients don’t miss messages due to short-term network issues or refreshes.

### 🔄 How it works

- The server stores a **fixed-size buffer** of recent messages per topic (e.g., using a ring buffer).
- On resubscription, the server looks up the topic’s stored messages and sends them immediately after subscribing.

### 📦 Example Replay

Let’s say:

1. Client A subscribes to `chat`
2. Three messages are published to `chat`
3. Client A disconnects
4. Client A reconnects and resubscribes to `chat`

Client A will automatically receive the last 3 messages upon resubscription 🎉

> This feature is especially useful for clients in unstable networks or switching tabs/devices.

### ⚙️ Configuration

- You can customize the **replay buffer size** per topic (e.g., 10 messages) in the codebase.
- Optionally, extend this to support **persistent replay** via disk or database.

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
  "payload": "Hello from Tab 1!"
}
```

6. You should see that message appear in Tab 2 — 🎉 real-time pub/sub in action!

### Sending ACK Messages

When you receive a QoS 1 message (where `"qos": 1` is present in the message payload), the broker expects an acknowledgment from your client. If it doesn't receive one within the configured timeout, it will re-send the message.

To send an ACK message in WebSocket King:

1.  **Receive a QoS 1 message:** Observe the incoming message in WebSocket King. It will look something like this:
    ```json
    {
      "topic": "my_critical_topic",
      "payload": "Important message!",
      "timestamp": 1689475200,
      "message_id": "some_unique_message_id_123",
      "qos": 1
    }
    ```

2.  **Extract the `message_id`:** From the received message, copy the value of the `message_id` field (e.g., `"some_unique_message_id_123"`).

3.  **Construct and Send the ACK:** In WebSocket King, send a new message with the following JSON format, replacing `"your_message_id_here"` with the actual `message_id` you extracted:
    ```json
    {
      "type": "ack",
      "message_id": "your_message_id_here"
    }
    ```

Once the broker receives this ACK, it will stop re-sending that specific message.

## Next Steps



🔐 Add authentication and access control

🌐 Build a web UI or API client

## 📜 License

Apache 2.0
