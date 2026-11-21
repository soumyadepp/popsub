# PopSub

A publish/subscribe broker over WebSockets, written in Rust. One binary, a data directory, and a JSON protocol any language can speak.

Every message gets a sequence number in its topic's log, so subscribers can replay history, resume where they left off after a disconnect or a server restart, and spot duplicates. QoS 1 messages are acknowledged per subscriber and retried until they are.

```
cargo run -p popsub -- server
```

## What you get

- **Topic logs with sequence numbers.** Replay the last N messages, everything since a time, or everything from a `seq`. Replay and live delivery join with no gap and no repeat.
- **Wildcard subscriptions.** Subscribe to `orders/#` or `sensors/+/temp` and get every matching topic you may read, including ones created later, each message under its own topic and number.
- **Shared subscriptions.** Subscribe with a group name and each message goes to one member of the group, so workers can split a queue. The group remembers its position, and a worker that dies hands its unfinished messages to the others.
- **Durable sessions.** Name a session when you connect, and your subscriptions and acknowledgement positions survive disconnects and server restarts.
- **QoS 0 and QoS 1.** QoS 1 is at-least-once per subscriber. A publisher that retries with the same `message_id` doesn't create a duplicate, and gets the original `seq` back.
- **Storage you choose.** A file backend (the default), an in-memory one, or your own through the `Storage` trait, held to a shared conformance suite.
- **Users you can manage.** Create accounts, grant and revoke topic patterns, and disable or delete accounts with `popsub users` or the admin requests. Changes reach live connections at once: a revoke ends the subscriptions it no longer allows, and disabling an account disconnects it.
- **Safe defaults.** Users with per-topic permissions, Argon2id passwords, JWTs, rate limits on logins, registration and publishing, size limits, heartbeats, and backpressure for slow readers.
- **A Rust client** (`popsub_sdk`) that handles reconnection, resubscription and deduplication for you, and a JSON Schema for building clients in other languages.

## Install

| What | How |
|---|---|
| Server, prebuilt | Download a `.tar.gz` for Linux (x86_64, aarch64) or macOS (Apple silicon) from [Releases](https://github.com/soumyadepp/popsub/releases), check it against its `.sha256`, and run `./popsub server` |
| Server, container | `docker run -p 8080:8080 -v popsub-data:/data -e SERVER__JWT_SECRET=... -e SERVER_PASSWORD=... ghcr.io/soumyadepp/popsub:0.1` |
| Rust client | `cargo add popsub_sdk` |
| TypeScript client | `npm install popsub` |
| Python client | `pip install popsub` |
| Go client | `go get github.com/soumyadepp/popsub/clients/go@v0.1.0` |

The container listens on all interfaces, so the server refuses to start until you set your own JWT secret and admin password; the defaults are published. It keeps its data in the `/data` volume, serves the health endpoints on port 9090, and runs as an unprivileged user. Any setting can be passed as an environment variable (see [Configuration](#configuration)).

Version 0.x: the protocol and the client APIs may still change between minor versions. [`CHANGELOG.md`](CHANGELOG.md) lists what changed in each release.

## Quick start

To build from source you need Rust 1.89 or newer.

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

```bash
cargo add popsub_sdk
```

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

[`examples/`](examples) has a subscriber and a publisher for each client library, with a walkthrough that runs them against a local server.

## Use it from any language

Client libraries for other languages live in [`clients/`](clients), each held to the same behaviour by one shared scenario suite (server restarts, dropped connections, duplicates, manual acks, revoked access). [`clients/README.md`](clients/README.md) says what every client must do.

To write your own, the protocol is JSON over a WebSocket. A session looks like this (`→` is the client, `←` the server):

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
| QoS 1 | At least once to every connected subscriber, independently. Each subscriber's acknowledgement clears only its own copy. A message that runs out of retries pauses that subscription and tells the client, instead of disappearing; or, with `broker.dead_letter_topic` set, is copied to that topic and the subscription carries on. |
| Durable sessions | At least once across disconnects, restarts of the application and server restarts, as long as the session reconnects within `session_expiry_secs` and the messages are still retained. |
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
| `broker.dead_letter_topic` | none | Copy messages that run out of retries here and keep the subscription going, instead of pausing it |
| `storage.retention.*` | 1000 messages, 1 hour, 64 MiB | History kept per topic |
| `[[storage.topic_retention]]` | none | Different limits for topics matching a pattern, such as `audit/#` for 30 days; first match wins |
| `server.max_payload_bytes` | 1 MiB | Largest publish payload. A binary body travels as base64, so it counts as about a third more |
| `server.proxy_protocol` | `false` | Read PROXY protocol headers from trusted TCP load balancers |
| `server.trusted_proxies` | none | Proxies whose `X-Forwarded-For` is believed; see [Running behind a proxy](#running-behind-a-proxy) |
| `broker.max_connections` | `1000` | Connections at once; more are told to try again later. `0` means no limit |
| `server.max_connections_per_address` | `0` (no limit) | Connections one client address may hold. Set `trusted_proxies` first if you run behind a proxy |
| `telemetry.mask_pii` | `true` | Mask usernames, client addresses and session ids in logs and traces |
| `telemetry.enabled`, `telemetry.sample_ratio` | `false`, `1.0` | Export metrics, traces and logs over OTLP; see [Metrics, traces and logs](#metrics-traces-and-logs) |
| `server.health_address` | none | Serve `/healthz` and `/readyz` here, such as `0.0.0.0:9091` |
| `server.tls_cert`, `server.tls_key` | none | PEM certificate chain and key; set both to serve `wss://` |
| `limits.*` | see section 9 | Failed logins before a lockout, lockout lengths, registrations per hour, publishes per second |

Topics can keep different amounts of history:

```toml
[[storage.topic_retention]]
pattern = "audit/#"
max_age_secs = 2592000   # 30 days; other limits come from [storage.retention]

[[storage.topic_retention]]
pattern = "presence/+"
max_messages = 1
```

Every setting, with what it does, is in [`docs/DESIGN.md`](docs/DESIGN.md) section 9.

## Health checks

Set `server.health_address` (say `0.0.0.0:9091`) and PopSub serves two endpoints over plain HTTP, apart from the WebSocket port:

- `GET /healthz` answers 200 while the process is up. Use it for liveness.
- `GET /readyz` answers 200 when PopSub should get traffic, and 503 with a reason while it is shutting down or while topic appends are failing (a full disk, say). Use it for readiness and load balancer health checks.

PopSub shuts down cleanly on Ctrl+C or `SIGTERM` (what `docker stop` and Kubernetes send): readiness turns to 503 and every client is told `going_away` before the process exits.

## Metrics, traces and logs

PopSub exports all three with OpenTelemetry, over OTLP (HTTP and protobuf). It is off until you turn it on:

```toml
[telemetry]
enabled = true
sample_ratio = 1.0   # share of request traces kept
```

Where the data goes is set with the standard variables: `OTEL_EXPORTER_OTLP_ENDPOINT` (default `http://localhost:4318`), `OTEL_SERVICE_NAME` (default `popsub`) and `OTEL_RESOURCE_ATTRIBUTES`. Point it at an OpenTelemetry Collector, which can forward to any backend, over TLS if needed; PopSub itself only sends plain `http://`. If nothing is listening, exports are dropped and the server carries on. Log lines still go to standard output either way.

To look at everything on one machine:

```bash
docker run -p 3000:3000 -p 4318:4318 grafana/otel-lgtm
TELEMETRY_ENABLED=true make server   # then open http://localhost:3000
```

What you get:

- **Metrics** (every 60 seconds; `OTEL_METRIC_EXPORT_INTERVAL` in milliseconds changes it): connections (open, opened, refused and closed, with reasons), logins by outcome, error frames by code, messages published, delivered, redelivered and acknowledged, stalled subscriptions, messages in flight, topics, subscriptions, publish latency, and storage errors. The full list is in [`docs/DESIGN.md`](docs/DESIGN.md) section 3.3.
- **Traces**: one span per client request (acknowledgements aside), named `popsub publish`, `popsub subscribe` and so on, marked as errors with the error code when the request fails.
- **Logs**: the same events as the console, linked to the trace of the request they happened in.

Personal data stays out by default. Passwords, tokens and message payloads are never logged. Usernames, client addresses and session ids are masked everywhere, console included, as `masked:3f2a9c1b`: the same value always masks the same way while the server runs, so you can still follow one user's events, but the name can't be read back. Set `telemetry.mask_pii = false` to see them while debugging. Topic names are not masked, so keep personal data out of them.

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

Admins can also look at and empty topics:

```bash
popsub topics list            # name, first and last seq, messages kept, live subscribers
popsub topics show orders
popsub topics purge orders    # removes its messages; numbering carries on
```

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
| `popsub` | The binary: `server`, a demo `client`, and `users` and `topics` for managing a running server |
| `popsub_sdk` | The Rust client library |
| `popsub_scenarios` | The scenario suite every client library must pass, and the Rust client's driver for it |
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

Releases are cut by pushing a version tag; [`docs/RELEASING.md`](docs/RELEASING.md) has the steps.

## Contributing

Contributions are welcome. [`CONTRIBUTING.md`](CONTRIBUTING.md) covers setting up, how to make a change, what each kind of change must also update (protocol, delivery behaviour, settings, storage), the code style, and what a pull request needs.

## License

Apache 2.0. See [`LICENSE`](LICENSE).
