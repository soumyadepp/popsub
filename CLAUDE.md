# PopSub

A single-process WebSocket publish/subscribe broker in Rust, with users, per-topic permissions and rate limits. Read `docs/DESIGN.md` before changing behaviour: it describes the protocol, the delivery guarantees as they really are, and the planned direction. Read `docs/STYLE.md` before writing any code or docs.

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
| `crates/popsub_transport` | WebSocket server, JSON protocol, and enforcing auth, permissions and rate limits |
| `crates/popsub_auth` | Users, roles, Argon2id passwords, JWTs, topic permission patterns |
| `crates/popsub_ratelimit` | Token bucket, sliding window and login lockout limiters |
| `crates/popsub_broker` | Topics, routing, QoS 1 retries, replay |
| `crates/popsub_persistence` | `sled` storage of recent messages per topic |
| `crates/popsub_client` | `Client`, the server-side record of one connection (not a client library) |
| `crates/popsub_config` | Settings from `config/default.toml` and env vars |
| `crates/popsub_utils` | `PopSubError`, `Result`, logging |

Each crate has its own `CLAUDE.md` with its invariants and gotchas.

Dependencies only point downwards: binary, then transport, then broker, auth and ratelimit, then persistence and client, with config and utils at the bottom. Never make a lower crate depend on a higher one. The broker must not know about sockets or JSON protocol messages.

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
- **New settings** go in `popsub_config/src/settings.rs` (both the full and `Partial` structs), the defaults, the merge in `load_config`, `config/default.toml`, and section 9 of the design doc. Keys containing an underscore are set from the environment with a double underscore after the section (`SERVER__JWT_SECRET`).
- **Security defaults stay closed.** Registration off, new users with no topics, published secrets refused on public addresses. Don't loosen these without a setting that defaults to the closed behaviour.
- Don't commit generated or local files: `target/`, `pubsub_db/`, coverage output such as `*.profraw` and `cobertura.xml`, or `.env`.

## Gotchas

- Delivered messages have no `type` field, unlike every other server frame. Clients detect them by its absence. See design doc section 4.2.
- QoS 1 is only at-least-once for a single subscriber that stays connected. Don't describe it more strongly in docs or READMEs.
- `broker.max_connections` and `broker.message_ttl_secs` are loaded but not used. Persistence uses `Persistence::default()` with a hard-coded `pubsub_db` path.
- The JWT secret default differs between code (`default_secret`), `config/default.toml` (`a_very_secret_key`) and the auth crate (`change_me_in_production`). All three are treated as published; the server refuses them on non-loopback hosts.
- Rate limits key on the socket address. Behind a proxy every client shares one address.
- There's no protocol message to grant topics, so self-registered users with the default `none` role can do nothing until their role is changed in code.
- `popsub_utils` depends on `config` 0.13 while `popsub_config` uses 0.15.
- Locally, port 8080 is often taken by another dev server. Use `PORT=8090`.

## Git

- Work on a branch and open a pull request into `main`. Don't push to `main` directly.
- Ask before committing or pushing.
