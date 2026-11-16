# PopSub

A publish/subscribe broker over WebSockets, written in Rust. One binary, a data directory, and a JSON protocol any language can speak.

Every message gets a sequence number in its topic's log, so subscribers can replay history, resume where they left off after a disconnect or a server restart, and spot duplicates. QoS 1 messages are acknowledged per subscriber and retried until they are.

```
cargo run -p popsub -- server
```

## What you get

- **Topic logs with sequence numbers.** Replay the last N messages, everything since a time, or everything from a `seq`. Replay and live delivery join with no gap and no repeat.
- **Durable sessions.** Name a session when you connect, and your subscriptions and acknowledgement positions survive disconnects and server restarts.
- **QoS 0 and QoS 1.** QoS 1 is at-least-once per subscriber. A publisher that retries with the same `message_id` doesn't create a duplicate, and gets the original `seq` back.
- **Storage you choose.** A file backend (the default), an in-memory one, or your own through the `Storage` trait, held to a shared conformance suite.
- **Safe defaults.** Users with per-topic permissions, Argon2id passwords, JWTs, rate limits on logins, registration and publishing, size limits, heartbeats, and backpressure for slow readers.
- **A Rust client** (`popsub_sdk`) that handles reconnection, resubscription and deduplication for you, and a JSON Schema for building clients in other languages.

## Quick start

You need Rust 1.89 or newer.

```bash
git clone https://github.com/soumyadepp/popsub.git
cd popsub
make server
```

In another terminal:

```bash
make client
```

The client logs in as the default admin, subscribes to `chat`, publishes a message with confirmation, and prints it when it arrives. On a fresh data directory you'll see:

```
Logged in as admin
Authenticated
Subscribed to chat
Published as seq 1
Received on chat (seq 1): Hello from popsub client
```

`make demo PORT=8090` does both in one go. The default admin is `admin` / `password`, which only works on `localhost`: see [Configuration](#configuration) before exposing the server.

## Use it from Rust

```rust
use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

let config = ClientConfig::new("ws://127.0.0.1:8080", Credentials::password("alice", "secret"))
    .session("alice-laptop");
let client = Client::connect(config).await?;

let mut orders = client.subscribe("orders", StartFrom::Resume).await?;
let stored = client.publish_confirmed("orders", "order 42 shipped").await?;

while let Some(message) = orders.next().await {
    println!("{} #{}: {}", message.topic, message.seq, message.payload);
}
```

The client logs in, refreshes its token, and reconnects with backoff. It knows from the server's close code when not to reconnect, and resubscribes from the next message it expects. It resends confirmed publishes without duplicating them, acknowledges QoS 1 messages (automatically, or when you call `message.ack()`), and never hands you the same `(topic, seq)` twice within one client. `client.events()` tells you about disconnects, truncated history and stalled subscriptions.

## Use it from any language

The protocol is JSON over a WebSocket. A session looks like this (`→` is the client, `←` the server):

```text
→ {"type": "login", "id": 1, "username": "alice", "password": "secret"}
← {"type": "logged_in", "id": 1, "token": "eyJ..."}
→ {"type": "auth", "id": 2, "token": "eyJ...", "protocol": 1, "session_id": "alice-laptop"}
← {"type": "authenticated", "id": 2, "protocol": 1, "session_id": "alice-laptop", "limits": {...}}
→ {"type": "subscribe", "id": 3, "topic": "orders", "from": {"last": 10}}
← {"type": "subscribed", "id": 3, "topic": "orders", "replay_from": 33, "replay_to": 42}
← {"type": "message", "topic": "orders", "seq": 33, "message_id": "...", "payload": "...", "qos": 1, "published_at_ms": 1794800000000}
→ {"type": "ack", "topic": "orders", "seq": 33}
→ {"type": "publish", "id": 4, "topic": "orders", "payload": "order 43 placed", "qos": 1}
← {"type": "published", "id": 4, "message_id": "...", "seq": 43}
```

- Any request may carry an `id`, which its reply echoes.
- Every failure is an `error` frame with a `code` such as `not_authorized` or `rate_limited`.
- When the server closes a connection, the WebSocket close code says why and whether to reconnect.

Where to find the full protocol:

- [`docs/protocol/`](docs/protocol/): JSON Schemas for every client and server frame, plus the close codes, generated from the server's own types. Generate your client's types from these.
- [`docs/DESIGN.md`](docs/DESIGN.md) section 4: what each frame means, the error and close codes, and the connection lifecycle.

## Delivery guarantees

| | Guarantee |
|---|---|
| QoS 0 | At most once, to subscribers connected when it is published. |
| QoS 1 | At least once to every connected subscriber, independently. Each subscriber's acknowledgement clears only its own copy. A message that runs out of retries pauses that subscription and tells the client, instead of disappearing. |
| Durable sessions | At least once across disconnects and server restarts, as long as the session reconnects within `session_expiry_secs` and the messages are still retained. |
| Ordering | Per topic, in `seq` order. A retry of an unacknowledged message can arrive after later ones. |
| Duplicates | Possible, and always detectable by `(topic, seq)`. The Rust client drops them for you. |
| Retention | Each topic keeps `max_messages`, `max_age_secs` and `max_bytes` of history. A subscriber that asks for trimmed history is told with a `truncated` frame. |

PopSub is a single process. There is no clustering or replication, and no TLS in the process: run it behind a TLS-terminating proxy. For a production system that needs those today, use NATS or Redis Streams. PopSub is small enough to read end to end, and its guarantees are written down precisely.

## Configuration

Settings come from `config/default.toml`, then environment variables, which win. A one-word key takes one underscore after its section (`SERVER_PORT=9000`). A key containing an underscore, or a nested section, takes two (`SERVER__JWT_SECRET=...`, `STORAGE__RETENTION__MAX_MESSAGES=5000`).

The settings you are most likely to change:

| Setting | Default | |
|---|---|---|
| `server.host`, `server.port` | `127.0.0.1`, `8080` | Where to listen |
| `server.jwt_secret`, `server.password` | published defaults | Change both before listening anywhere but `localhost`; the server refuses to start otherwise |
| `server.allow_registration` | `false` | Let clients create accounts; `server.registration_role` decides what they may use |
| `storage.backend` | `file` | `file` keeps history and sessions in `storage.path` (`pubsub_data`); `memory` loses them on restart |
| `storage.durability` | `buffered` | `buffered` survives a server crash; `synced` also survives a power cut, at the cost of a disk flush per publish |
| `storage.retention.*` | 1000 messages, 1 hour, 64 MiB | History kept per topic |
| `server.max_payload_bytes` | 1 MiB | Largest publish |

Every setting, with what it does, is in [`docs/DESIGN.md`](docs/DESIGN.md) section 9.

## Project layout

| Crate | What it is |
|---|---|
| `popsub` | The binary: `server` and a demo `client` |
| `popsub_sdk` | The Rust client library |
| `popsub_protocol` | Every frame type, shared by the server and clients |
| `popsub_transport` | The WebSocket server: authentication, permissions, rate limits, connection lifecycle |
| `popsub_broker` | Topic writers, delivery, replay, acknowledgements, sessions |
| `popsub_storage` | Topic logs and sessions: the `Storage` traits, file and memory backends, the conformance suite |
| `popsub_auth` | Users, roles, passwords, tokens, topic permissions |
| `popsub_ratelimit` | Token bucket, sliding window and login lockout limiters |
| `popsub_connection` | The server's record of one connection and its outgoing queue |
| `popsub_config` | Settings |
| `popsub_utils` | Shared errors and logging |

## Development

```bash
make            # list every target
make test       # all tests
make ci         # what CI runs: format check, clippy, tests, schema check
make demo       # start a server, run the client against it, stop
make schema     # regenerate docs/protocol/ after changing a protocol type
```

Add `JOBS=2` to any target if a full build runs out of memory. Before changing code, read [`docs/STYLE.md`](docs/STYLE.md); before changing behaviour, read [`docs/DESIGN.md`](docs/DESIGN.md). The storage design is in [`docs/STORAGE.md`](docs/STORAGE.md).

## License

Apache 2.0. See [`LICENSE`](LICENSE).
