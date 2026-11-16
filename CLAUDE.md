# PopSub

A single-process WebSocket publish/subscribe broker in Rust, with users, per-topic permissions and rate limits. Read `docs/DESIGN.md` before changing behaviour: it describes the protocol, the delivery guarantees as they really are, and the planned direction. Read `docs/STYLE.md` before writing any code or docs. `docs/STORAGE.md` is the proposed direction for storage; check it before changing persistence, acks or replay.

## Style rules (enforced, read before every change)

- **Google-style docstrings, everywhere.** One `//!` module docstring at the top of every `.rs` file. One `///` docstring on every function (private, trait and test functions included), struct, enum, trait, type alias, const and static. Sections are plain labels with entries indented four spaces: `Args:` (every parameter except `self`), `Returns:` (unless `()`), `Raises:` (each error variant, for every function returning `Result`). Test functions get one sentence. Full guide and example in `docs/STYLE.md`.
- **No inline comments.** No `//` comments anywhere, tests and examples included. No separators, numbered steps or given/when/then. If something needs explaining, it goes in the docstring of the enclosing item.
- **No rustdoc headings** (`# Arguments`, `# Errors`, `# Example`). Use `Example:` above a fenced block so doc tests still run.
- **No slop.** No emoji in code or docs. No stock phrases such as "This ensures", "robust", "comprehensive", "seamless", "leverage", "production-ready", "for backward compatibility". No em or en dashes. Docstrings say specifically what the item does and why, never that it is good, and never just restate the name.
- **Accurate over flattering.** Docs must match the code. If a guarantee is partial, say so.
- Check before finishing: `grep -rnE '^\s*//[^/!]' crates` prints nothing, and every `.rs` file starts with `//!`.

## Commands

Use the Makefile. `make` lists everything.

| Task | Command |
|---|---|
| Run the server | `make server` (override with `HOST=... PORT=...`) |
| End-to-end smoke test | `make demo PORT=8090` |
| All tests | `make test` |
| One crate | `make test-crate CRATE=popsub_broker` |
| What CI runs | `make ci` (fmt check, clippy with `-D warnings`, tests) |
| Format | `make fmt` |
| Wipe stored messages | `make reset-data` |

Builds can run out of memory on a laptop when everything compiles at once. Add `JOBS=2` to any target if a build gets killed.

## Workspace

| Crate | Owns |
|---|---|
| `crates/popsub` | The binary and CLI wiring |
| `crates/popsub_protocol` | Every frame type, error codes and `PROTOCOL_VERSION`. Depends only on `serde` |
| `crates/popsub_transport` | WebSocket server, and enforcing auth, permissions and rate limits |
| `crates/popsub_auth` | Users, roles, Argon2id passwords, JWTs, topic permission patterns |
| `crates/popsub_sdk` | The Rust client library: reconnection, durable sessions, acknowledgements, deduplication. The CLI's `client` uses it |
| `crates/popsub_ratelimit` | Token bucket, sliding window and login lockout limiters |
| `crates/popsub_broker` | Topic writers, routing, replay, QoS 1 retries |
| `crates/popsub_storage` | Topic logs, subscription cursors, the `Storage` traits, the `file` and `memory` backends and the conformance suite |
| `crates/popsub_connection` | `Client` and `Outbox`: the server's record of one connected socket and its outgoing queue |
| `crates/popsub_config` | Settings from `config/default.toml` and env vars |
| `crates/popsub_utils` | `PopSubError`, `Result`, logging, and every OpenTelemetry metric instrument (`metrics`) |

Each crate has its own `CLAUDE.md` with its invariants and gotchas.

Dependencies only point downwards: binary, then transport, then broker, auth and ratelimit, then client and config, with storage and utils at the bottom. Never make a lower crate depend on a higher one. `popsub_protocol` sits beside them and is used by transport, broker and binary. The broker writes protocol frames to channels but must not know about sockets or the transport's types.

## Conventions

- Rust 2024 edition. Let-chains (`if let ... && let ...`) are used and fine.
- `cargo fmt` and `cargo clippy --all-targets --all-features -- -D warnings` must pass. CI fails on any warning.
- Unit tests live in a `tests.rs` module per crate, wired with `#[cfg(test)] mod tests;`. Transport has `websocket_tests.rs` for tests that start a real server on a free port via `portpicker`.
- Errors go through `popsub_utils::error::PopSubError`. Don't add `unwrap()` on paths that handle client input.
- Log with `tracing` and structured fields (`client_id = %id`), not `println!`. The CLI client in `crates/popsub` is the exception.
- Never log passwords, tokens, payloads or raw frame text. Wrap every username, client address, rate limiter key and session id in `popsub_utils::pii::pii` as a field (`username = %pii(username)`), never inside the message text, and log auth errors as `e.kind()`, not `%e`. `crates/popsub_transport/tests/pii.rs` fails if a session's logs show any of them; extend it when you add a log path that handles identities.
- Record metrics through `popsub_utils::metrics::metrics()`, and add any new instrument there and to the table in section 3.3 of `docs/DESIGN.md`. Library crates use only the OpenTelemetry API, never the SDK.
- Keep doc comments (`//!` and `///`) accurate when behaviour changes. Several describe guarantees the code doesn't fully meet; fix the comment or the code, never leave them disagreeing.

## Rules for changes

- **Protocol changes** touch `popsub_protocol` (types and its wire tests), section 4 of `docs/DESIGN.md`, the WebSocket tests, the regenerated `docs/protocol/` schema, and the README's example session if it shows the changed frames, in the same change. Within a protocol version only add frames, fields and error codes; anything else needs a new `PROTOCOL_VERSION`.
- **After changing any protocol type, run `make schema`** and commit the regenerated `docs/protocol/*.json`. CI's schema check fails otherwise.
- **Every failure a client sees is an `error` frame with an `ErrorCode`.** Don't send free-text-only errors, and echo the request's `id` in every reply.
- **Delivery semantics changes** must update the table in section 6 of `docs/DESIGN.md` and come with a test that demonstrates the new guarantee, written to fail first.
- **Reach the broker only through `SharedBroker::with`** (inside the broker crate, `BrokerCell::with`, which it wraps). The closure can't await, so the lock can't be held across an `.await`. Don't add another way to lock it.
- **Storage I/O belongs in the topic writer**, never under the broker lock. Publishes and subscribes for a topic go through its writer so they are handled one at a time; don't add a path that appends or replays around it.
- **New settings** go in `popsub_config/src/settings.rs` (the field and its struct's `Default`), `config/default.toml`, and section 9 of the design doc. Keys containing an underscore are set from the environment with a double underscore after the section (`SERVER__JWT_SECRET`).
- **Security defaults stay closed.** Registration off, new users with no topics, published secrets refused on public addresses. Don't loosen these without a setting that defaults to the closed behaviour.
- Don't commit generated or local files: `target/`, `pubsub_data/`, `pubsub_db/`, coverage output such as `*.profraw` and `cobertura.xml`, or `.env`.

## Gotchas

- Deliveries, `subscribed`, `truncated` and `stalled` are written by the broker, not the transport, using the same `popsub_protocol::ServerFrame`.
- A subscribe is answered by a `subscribed` frame from the broker, but a QoS 0 publish and an `ack` have no reply.
- Connections get WebSocket pings. Test helpers that read frames must skip pings (`next_text` in `websocket_tests.rs` does), and a test client that stops reading for longer than the idle timeout will be closed.
- Every close the server starts must go through `Flow::Close(CloseCode)` (or `close_message` in the broker), never a bare drop, so clients get a code. Tests that need a connection's earlier frames handled publish to a topic of their own and wait for it to come back (`settle` in `websocket_tests.rs`).
- Durable sessions are keyed by `session_key(username, session_id)`. Never key them by the client's `session_id` alone, or one user could attach to another's session.
- QoS 1 is at-least-once per subscriber while connected, and across reconnects only for durable sessions whose records are still retained. With the `file` backend (the default) sessions and history survive restarts; with `memory` nothing does. Don't describe it more strongly in docs or READMEs.
- `broker.max_connections` is enforced by the transport, counting from `accept`; connections over it are closed with `TryAgainLater` (1013). `0` means no limit. Tests that open many connections need a higher limit.
- The `file` backend locks `storage.path` (`pubsub_data` by default). Two servers can't share a working directory: the second exits. Tests use a temporary directory or `MemoryStorage`, never `pubsub_data`.
- The JWT secret default differs between code (`default_secret`), `config/default.toml` (`a_very_secret_key`) and the auth crate (`change_me_in_production`). All three are treated as published; the server refuses them on non-loopback hosts.
- Rate limits key on the client address: the socket peer, or, for peers listed in `server.trusted_proxies`, the rightmost untrusted `X-Forwarded-For` entry (`popsub_transport::proxy`), starting from the PROXY protocol header's address when `server.proxy_protocol` is on. Never believe either header from a peer that isn't listed.
- Rate limits come from the `[limits]` section through `RateLimiters::from_settings`; `RateLimiters::default` is the same values.
- Accounts are managed with the admin requests (`create_user`, `grant`, `revoke`, `set_enabled` and so on), which only an enabled admin may send; `popsub users` sends them. They are handled in `popsub_transport/src/websocket/admin.rs`, which also ends live subscriptions a change no longer allows and closes disabled accounts with `AccountDisabled`. The last enabled admin can't be removed.
- Locally, port 8080 is often taken by another dev server. Use `PORT=8090`.

## Git

- Work on a branch and open a pull request into `main`. Don't push to `main` directly.
- Ask before committing or pushing.
