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
- **Users you can manage.** Create accounts, grant and revoke topic patterns, and disable or delete accounts with `popsub users` or the admin requests. Changes reach live connections at once: a revoke ends the subscriptions it no longer allows, and disabling an account disconnects it.
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

## How it works

PopSub rests on five ideas. Once they click, the rest of the code is plumbing.

1. **Every topic is an append-only log.** A publish is appended and numbered with the topic's next `seq` (1, 2, 3, ...). The log lives in a `Storage` backend: by default, checksummed segment files under `pubsub_data/`.
2. **Every subscription is a position in a log.** For each subscription the broker remembers the next `seq` it expects and the QoS 1 messages it hasn't acknowledged yet (its window). A durable session also stores that position, so it can pick up where it left off after a disconnect or a restart.
3. **Each topic has one writer.** Every publish, subscribe and catch-up for a topic goes through that topic's writer task, one at a time. That is what keeps delivery in order and makes replay join live delivery with no gap, without holding a lock during disk I/O.
4. **Falling behind means reading from the log.** A subscription is only ever sent the exact `seq` it expects next. When it can't take more (its window is full, or its connection's outgoing queue is backed up), it falls behind, and once it is ready the writer reads what it missed back from the log. Nothing piles up in memory for a slow subscriber.
5. **The protocol always says what happened.** Replies carry the request's `id`, errors carry codes, and the server's closes carry close codes, so a client always knows what to do next.

The path of one message:

```text
publisher ──publish──▶ transport ───────────▶ topic writer ──append──▶ storage (topic log)
                       checks auth,                │
                       permission, rate            │ deliver to every subscription that
                       limit and size              │ expects this seq and has room
                                                   ▼
subscriber ◀──message (topic, seq)── outbox ◀── broker
subscriber ──ack (topic, seq)──────────────────▶ broker: clears it from that subscription's
                                                 window, moves its position, stores the
                                                 position for a durable session
```

Where to start reading the code:

| File | What's there |
|---|---|
| `crates/popsub_transport/src/websocket.rs` | One connection's life: login and auth, a handler per request, heartbeats, token expiry, closing |
| `crates/popsub_broker/src/writer.rs` | The topic writer: publish, subscribe with replay, catch-up |
| `crates/popsub_broker/src/engine.rs`, `topic.rs` | Subscriptions, windows, acknowledgements, retries |
| `crates/popsub_storage/src/storage.rs` | The storage traits; `file/` is the default backend |
| `crates/popsub_protocol/src/` | Every frame, error code and close code |
| `crates/popsub_sdk/src/driver.rs` | The client side of all of the above |

The full design, with the reasons behind it, is in [`docs/DESIGN.md`](docs/DESIGN.md) and [`docs/STORAGE.md`](docs/STORAGE.md).

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

PopSub is a single process. There is no clustering or replication. For a production system that needs those today, use NATS or Redis Streams. PopSub is small enough to read end to end, and its guarantees are written down precisely.

## Configuration

Settings come from `config/default.toml`, then environment variables, which win. A one-word key takes one underscore after its section (`SERVER_PORT=9000`). A key containing an underscore, or a nested section, takes two (`SERVER__JWT_SECRET=...`, `STORAGE__RETENTION__MAX_MESSAGES=5000`).

The settings you are most likely to change:

| Setting | Default | |
|---|---|---|
| `server.host`, `server.port` | `127.0.0.1`, `8080` | Where to listen |
| `server.jwt_secret`, `server.password` | published defaults | Change both before listening anywhere but `localhost`; the server refuses to start otherwise |
| `server.allow_registration` | `false` | Let clients create accounts; `server.registration_role` decides what they may use |
| `storage.backend` | `file` | `file` keeps history, sessions and user accounts in `storage.path` (`pubsub_data`); `memory` loses them on restart |
| `storage.durability` | `buffered` | `buffered` survives a server crash; `synced` also survives a power cut, at the cost of a disk flush per publish |
| `storage.retention.*` | 1000 messages, 1 hour, 64 MiB | History kept per topic |
| `server.max_payload_bytes` | 1 MiB | Largest publish |
| `server.proxy_protocol` | `false` | Read PROXY protocol headers from trusted TCP load balancers |
| `server.trusted_proxies` | none | Proxies whose `X-Forwarded-For` is believed; see [Running behind a proxy](#running-behind-a-proxy) |
| `broker.max_connections` | `1000` | Connections at once; more are told to try again later. `0` means no limit |
| `server.max_connections_per_address` | `0` (no limit) | Connections one client address may hold. Set `trusted_proxies` first if you run behind a proxy |
| `server.tls_cert`, `server.tls_key` | none | PEM certificate chain and key; set both to serve `wss://` |
| `limits.*` | see section 9 | Failed logins before a lockout, lockout lengths, registrations per hour, publishes per second |

Every setting, with what it does, is in [`docs/DESIGN.md`](docs/DESIGN.md) section 9.

## TLS

Point PopSub at a certificate and key and it serves `wss://`:

```toml
[server]
host = "0.0.0.0"
port = 443
tls_cert = "/etc/popsub/fullchain.pem"
tls_key = "/etc/popsub/privkey.pem"
```

The files are read at startup, so a renewed certificate takes a restart. Clients connect to `wss://your-host`; the Rust client trusts the public web roots, and `ClientConfig::add_root_certificates_pem` (or `--ca-cert` on the CLI) adds your own, such as a self-signed one for development.

## Running behind a proxy

A proxy is optional. It is useful when you want certificates renewed without restarts, to share port 443 with other services, or to filter traffic before it reaches PopSub. If you use one, tell PopSub to believe the client address it forwards, or every client will share the proxy's address for rate limits:

```toml
[server]
host = "127.0.0.1"
trusted_proxies = ["127.0.0.1"]
```

With nginx on the same machine:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_read_timeout 120s;
}
```

A TCP load balancer (such as AWS's Network Load Balancer, or HAProxy in TCP mode) can't add `X-Forwarded-For`. Turn on its PROXY protocol option, list it in `trusted_proxies`, and set `proxy_protocol = true`: PopSub then reads the client's address from the header it sends. Every connection from a listed proxy must carry one; connections from anywhere else are served as usual.

Once the proxy is trusted, you can also cap connections per client with `server.max_connections_per_address`. `proxy_read_timeout` must be longer than `server.heartbeat_secs`, or the proxy will cut quiet connections. Entries in `trusted_proxies` can be CIDR ranges (`10.0.0.0/8`) for a fleet of proxies. Only list proxies you run: a client reaching PopSub directly could otherwise claim any address.

## Managing users

Accounts live in `users.json` in the data directory (with the default `file` backend) and survive restarts. Manage them on a running server as an admin:

```bash
export POPSUB_PASSWORD=password             # the admin's password; otherwise you're prompted
popsub users add alice --topic 'orders/#' --topic 'news/+:subscribe'
popsub users grant alice audit --access publish
popsub users list
popsub users revoke alice 'news/+'
popsub users role bob readonly --topic 'metrics/#'
popsub users passwd alice
popsub users disable alice                  # closes her connections; `enable` lets her back in
popsub users delete alice
```

From the source tree, run these as `cargo run -p popsub -- users ...`. New passwords come from the `POPSUB_NEW_PASSWORD` environment variable or a prompt that doesn't echo, never from an argument. Add `--url ws://host:port` for a server elsewhere and `--username` for another admin.

A grant is a topic pattern (`+` matches one level, a trailing `#` the rest) with `subscribe`, `publish` or `both`. A `user` account may use what its grants allow, a `readonly` one may only subscribe, and an `admin` may use every topic and manage accounts. The last enabled admin can't be disabled, deleted or demoted.

From Rust, the same requests are on `client.admin()`:

```rust
use popsub_sdk::{Access, RoleSpec, TopicGrant};

let admin = client.admin();
admin.create_user("alice", "a-long-password", RoleSpec::user(vec![TopicGrant::new("orders/#", Access::Both)])).await?;
admin.revoke("alice", "orders/#").await?;
```

## Project layout

| Crate | What it is |
|---|---|
| `popsub` | The binary: `server`, a demo `client`, and `users` for managing accounts |
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

Add `JOBS=2` to any target if a full build runs out of memory.

## Contributing

Contributions are welcome. [`CONTRIBUTING.md`](CONTRIBUTING.md) covers setting up, how to make a change, what each kind of change must also update (protocol, delivery behaviour, settings, storage), the code style, and what a pull request needs.

## License

Apache 2.0. See [`LICENSE`](LICENSE).
