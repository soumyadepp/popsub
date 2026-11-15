# popsub_utils

Shared building blocks: the error type, the `Result` alias and logging setup. Every other crate may depend on this one; it depends on none of them.

## Files

- `src/error.rs`: `PopSubError` (via `thiserror`) with variants for serialisation, JWT, WebSocket, I/O, lock, client, persistence and config errors, and `Result<T>`.
- `src/logging.rs`: `init(level)`, which sets up `tracing_subscriber` with a fixed maximum level. Safe to call more than once.

## Gotchas

- `init` ignores `RUST_LOG`. Switching to `EnvFilter` would let operators change log levels without a rebuild.
- `config` is pinned to 0.13 here but 0.15 in `popsub_config`, so `PopSubError::Config` can't wrap errors from the loader. Bump this to match when touching either.
- Keep this crate free of domain logic. If something knows about topics, messages or sockets, it belongs elsewhere.
