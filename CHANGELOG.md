# Changelog

Each release lists what changed for people running the server and people using a client library. Versions follow [Semantic Versioning](https://semver.org); while the version is 0.x, a minor release may change the protocol or a library's API, and the notes say so.

## [0.2.0] - 2026-11-21

Two changes can affect an upgrade from 0.1.0. Read "Upgrading from 0.1.0" first.

### Upgrading from 0.1.0

- **Shared subscription groups belong to one account.** A group used to be global: any account allowed to read a topic could join another account's group by naming it, and take a share of its records. Now two accounts naming the same group get a group each. Workers that split a topic must log in as the same account. Group positions stored by 0.1.0 aren't carried over, so each group starts again from its `from` the first time it is joined after upgrading; the old entries are removed after `storage.session_expiry_secs`.
- **A `publish` with a `qos` other than 0 or 1 is refused** with `bad_request`. It used to be stored and handled as QoS 0, with no sign that the requested level didn't exist.

### Server

- `storage.backend = "memory"` starts with the default settings. The default `buffered` durability runs as `none`, with a warning; it used to refuse to start until `storage.durability` was set to `none`. An explicit `synced` is still refused.
- Auth errors are logged by kind, never by their full message, which can carry a username. Three log lines printed it: creating the default admin, a refused token, and a failed account lookup at `auth`.

### Client libraries

- No code changes. Each library has two runnable examples, a durable subscriber and a publisher, and [`examples/README.md`](https://github.com/soumyadepp/popsub/blob/v0.2.0/examples/README.md) walks through running them against a local server, across languages.

### Documentation

- Every document was checked against the code and corrected. `docs/DESIGN.md` section 12 now lists, in priority order, what PopSub still lacks for production use by small applications and IoT devices.

## [0.1.0] - 2026-11-20

The first release.

### Server

- A WebSocket publish/subscribe broker in one binary. Every message gets a sequence number in its topic's log; subscribers replay the last N messages, everything since a time, or everything from a `seq`, and replay joins live delivery with no gap or repeat.
- QoS 0 and QoS 1. QoS 1 is at-least-once per subscriber, with per-subscriber acknowledgements, retries, and a stalled notice or a dead-letter topic when retries run out. A publisher retrying with the same `message_id` doesn't create a duplicate.
- Durable sessions that keep subscriptions and acknowledgement positions across disconnects, application restarts and server restarts. A client can ask the server to hold a session's topics until it subscribes (`hold_resumed`).
- Wildcard subscriptions (`orders/#`, `sensors/+/temp`) and shared subscriptions, where each message goes to one member of a group.
- Binary payloads, sent as base64 with `encoding: "base64"`.
- Users with per-topic permissions, Argon2id passwords and JWTs, managed with `popsub users` or the admin requests. Topics can be listed, inspected and purged with `popsub topics` or the topic admin requests. Rate limits on logins, registration and publishing, connection limits, heartbeats and backpressure for slow readers.
- A file storage backend (the default), an in-memory one, and per-topic retention.
- TLS, trusted proxies and the PROXY protocol, health endpoints, and OpenTelemetry metrics, traces and logs with PII masking.
- Prebuilt binaries for Linux (x86_64, aarch64) and macOS (Apple silicon), and a container image at `ghcr.io/soumyadepp/popsub`.

### Client libraries

- Rust: `popsub_sdk` and `popsub_protocol` on crates.io.
- TypeScript, for browsers and Node.js 22+: `popsub` on npm.
- Python 3.10+, asyncio: `popsub` on PyPI.
- Go 1.23+: `github.com/soumyadepp/popsub/clients/go`.

Each library reconnects with backoff, resubscribes without gaps, resends confirmed publishes without duplicating them, acknowledges QoS 1 messages and delivers each `(topic, seq)` once, and passes the same scenario suite.

### Known limits

- One server process: no clustering or replication.
- `server.max_payload_bytes` is measured on the payload string, so a binary body counts about a third more.

[0.2.0]: https://github.com/soumyadepp/popsub/releases/tag/v0.2.0
[0.1.0]: https://github.com/soumyadepp/popsub/releases/tag/v0.1.0
