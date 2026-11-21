# popsub_utils

Shared building blocks: the error type, the `Result` alias, logging setup, the OpenTelemetry metric instruments and PII masking. Every other crate may depend on this one; it depends on none of them.

## Files

- `src/error.rs`: `PopSubError` (via `thiserror`) with variants for serialisation, JWT, WebSocket, I/O, client and config errors, and `Result<T>`.
- `src/logging.rs`: `init(level)`, which sets up `tracing_subscriber` with a fixed maximum level. Safe to call more than once.
- `src/metrics.rs`: `metrics()`, the shared `Metrics` instruments, created from the global meter provider on first use, the gauge names the binary registers, and `attr` for one-attribute sets. Uses only the OpenTelemetry API.
- `src/pii.rs`: `pii(value)`, which prints a value as `masked:` and a keyed hash while masking is on (the default), and `set_masking` / `masking` for `telemetry.mask_pii`.

## Gotchas

- `init` ignores `RUST_LOG`. Switching to `EnvFilter` would let operators change log levels without a rebuild.
- `config` matches `popsub_config`'s version (0.15), so `PopSubError::Config` can wrap loader errors. Keep them in step.
- Keep this crate free of domain logic. If something knows about topics, messages or sockets, it belongs elsewhere.
