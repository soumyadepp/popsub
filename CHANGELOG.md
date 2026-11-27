# Changelog

Each release lists what changed for people running the server and people using a client library. Versions follow [Semantic Versioning](https://semver.org); while the version is 0.x, a minor release may change the protocol or a library's API, and the notes say so.

## [Unreleased]

### Server

- Back up a running server with `popsub backup`, or the new `backup` admin request. It copies every topic, the sessions and `users.json` into a new folder under `storage.backup_path` (default `pubsub_backups`; `/backups` in the container image) while publishing carries on. `BACKUP.json` is written last, so a folder without it is unfinished. Restoring is copying the folder back to `storage.path` with the server stopped. The `memory` backend refuses with the new error code `backup_unsupported`, and a name already used gets `backup_exists`.
- Every release is now tested against data written by each earlier one: a newer release opens an older release's data, and an older release can't be assumed to read a newer one's. `make upgrade-fixture` adds a release's data to the test when it is cut.

### Client libraries

- `admin().backup()` in the Rust client.

### Project

- `SECURITY.md` says how to report a vulnerability privately, what happens next, and which versions get fixes.
- The client frame parser and the PROXY protocol header reader are fuzzed. `make fuzz` runs a target locally, and the `Fuzz` workflow runs both on pull requests that touch them and every week.

## [0.3.0] - 2026-11-27

A performance release. The server stores and delivers many times more messages a second than 0.2.0 on the same machine; [`docs/BENCHMARKS.md`](https://github.com/soumyadepp/popsub/blob/main/docs/BENCHMARKS.md) has the numbers before and after, and beside NATS and Mosquitto. The protocol is unchanged and no client library changed. Four behaviours did change; read "Upgrading from 0.2.0" first.

### Upgrading from 0.2.0

- **Replies can arrive in a different order from requests.** A connection no longer waits for a publish to be stored before it reads the next frame. A publish that is refused straight away (a bad frame, no permission, the rate limit) can be answered before an earlier publish that is still being stored. Match replies by `id`, as the client libraries already do. A request other than `publish`, `ack` and `ping` still waits for the connection's earlier publishes.
- **A publish without a `message_id` is never treated as a retry.** The server gives it a random id, as before, but no longer looks that id up among earlier ones. Only an id the publisher chose makes a publish idempotent.
- **Trimmed records can come back after a crash.** With the `file` backend, retention no longer writes the topic's head file on every publish. After a stop that wasn't clean, up to one segment of records that had been trimmed is in the log again until the next trim removes it. Purging a topic is still written at once.
- **A 0.2.0 server can't read logs written by 0.3.0.** Records are now stored in a compact binary form. 0.3.0 reads logs written by earlier versions, but downgrading after writing with 0.3.0 isn't possible: keep a copy of `storage.path` if you might go back.
- **One log line per publish is gone.** The `published` line is now at `debug`. A publish gets a trace span only while `telemetry.enabled` is on, and the span covers accepting the publish, not storing it.

### Server

- Less memory: each connection's WebSocket read buffer is 16 KiB instead of 128 KiB, so 10,000 idle connections take about 390 MiB instead of 1.5 GiB, and each retained record costs about 50 bytes in memory instead of about 90.
- Stored records are about 40% smaller (about 180 bytes for a 128-byte payload, from about 300), so a busy server writes much less to disk, and replay reads them back without parsing JSON.
- Publishes waiting for the same topic are stored with one write and, with `synced` durability, one disk flush. A busy topic no longer pays a flush per message.
- The default retention no longer slows a full topic to the speed of a disk flush per publish.
- A topic's subscribers each get a waiting batch of messages in one socket write, and delivering to a large fan-out gives the broker lock up every 50 microseconds or so, so one busy topic doesn't hold the others up.
- Topic writers run on their own threads (named `popsub-writer`), apart from the threads that read sockets.
- A subscriber whose outgoing queue filled is caught up once the queue has drained to half its soft limit, and the log is read a page at a time with one file read.
- A connection's permission to publish to a topic is remembered until any account changes, instead of being read from the user store for every message. Revoking a grant or disabling an account still refuses the next publish.
- A panic while the broker lock is held no longer marks the lock as poisoned; the lock is released and the server carries on, as it did before by recovering from the poisoning.

### Tools

- `popsub-bench` (`make bench`) measures a release build: throughput, delivery latency, fan-out, replay, restart time, idle connections and isolation between topics. `--target nats`, `nats-jetstream` or `mosquitto` runs the comparable loads against those brokers. Before each scenario it waits for the disk to finish writing what the last one left, so one scenario doesn't slow the next.

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
