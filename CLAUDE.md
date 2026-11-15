# PopSub

A single-process WebSocket publish/subscribe broker in Rust. Read `docs/DESIGN.md` before changing behaviour: it describes the protocol, the delivery guarantees as they really are, and the planned direction.

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
| `crates/popsub_transport` | WebSocket server, JSON protocol, login and JWT auth |
| `crates/popsub_broker` | Topics, routing, QoS 1 retries, replay |
| `crates/popsub_persistence` | `sled` storage of recent messages per topic |
| `crates/popsub_client` | `Client`, the server-side record of one connection (not a client library) |
| `crates/popsub_config` | Settings from `config/default.toml` and env vars |
| `crates/popsub_utils` | `PopSubError`, `Result`, logging |

Each crate has its own `CLAUDE.md` with its invariants and gotchas.

Dependencies only point downwards: binary, then transport, then broker, then persistence and client, with config and utils at the bottom. Never make a lower crate depend on a higher one. The broker must not know about sockets or JSON protocol messages.

## Conventions

- Rust 2024 edition. Let-chains (`if let ... && let ...`) are used and fine.
- `cargo fmt` and `cargo clippy --all-targets --all-features -- -D warnings` must pass. CI fails on any warning.
- Unit tests live in a `tests.rs` module per crate, wired with `#[cfg(test)] mod tests;`. Transport has `websocket_tests.rs` for tests that start a real server on a free port via `portpicker`.
- Errors go through `popsub_utils::error::PopSubError`. Don't add `unwrap()` on paths that handle client input.
- Log with `tracing` and structured fields (`client_id = %id`), not `println!`. The CLI client in `crates/popsub` is the exception.
- Keep doc comments (`//!` and `///`) accurate when behaviour changes. Several describe guarantees the code doesn't fully meet; fix the comment or the code, never leave them disagreeing.

## Rules for changes

- **Protocol changes** touch `popsub_transport/src/message.rs`, the README protocol section, section 4 of `docs/DESIGN.md`, and the WebSocket tests, in the same change.
- **Delivery semantics changes** must update the table in section 6 of `docs/DESIGN.md` and come with a test that demonstrates the new guarantee, written to fail first.
- **Never hold the broker lock across an `.await`.** It is a `std::sync::Mutex` shared by every task.
- **New settings** go in `popsub_config/src/settings.rs` (both the full and `Partial` structs), the defaults, the merge in `load_config`, `config/default.toml`, and section 9 of the design doc.
- Don't commit generated or local files: `target/`, `pubsub_db/`, coverage output such as `*.profraw` and `cobertura.xml`, or `.env`.

## Gotchas

- Delivered messages have no `type` field, unlike every other server frame. Clients detect them by its absence. See design doc section 4.2.
- QoS 1 is only at-least-once for a single subscriber that stays connected. Don't describe it more strongly in docs or READMEs.
- `broker.max_connections` and `broker.message_ttl_secs` are loaded but not used. Persistence uses `Persistence::default()` with a hard-coded `pubsub_db` path.
- The JWT secret default differs between code (`default_secret`) and `config/default.toml` (`a_very_secret_key`).
- `popsub_utils` depends on `config` 0.13 while `popsub_config` uses 0.15.
- Locally, port 8080 is often taken by another dev server. Use `PORT=8090`.
- `feat/add-auth` and `feat/add-rate-limiter` hold unmerged work: a user store, per-topic permissions and rate limiters. Check them before building auth or rate limiting again.

## Git

- Work on a branch and open a pull request into `main`. Don't push to `main` directly.
- Ask before committing or pushing.
