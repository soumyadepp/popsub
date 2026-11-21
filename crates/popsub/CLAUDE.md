# popsub (binary)

The CLI. Loads config, refuses to start on a non-loopback host while published default secrets are set (warns on localhost), opens the `[storage]` backend (`open_storage`: `FileStorage` in `storage.path`, or `MemoryStorage`) and the matching user store (`open_users`: `users.json` in `storage.path`, or in memory), builds a `SharedBroker` (which refuses a durability the backend can't provide; `effective_durability` first runs the default `buffered` as `none` for the `memory` backend), loads TLS when `server.tls_cert` and `server.tls_key` are both set (`open_tls`; one without the other stops startup), applies the `[limits]` rate limits (`RateLimiters::from_settings`) and `server.trusted_proxies` (`TrustedProxies::parse`, an invalid entry stops startup), spawns its retry, retention and session expiry loops, serves the health endpoints on `server.health_address` when set, and runs `Server::from_settings_with_users(...).listen(...)` until Ctrl+C or `SIGTERM` (`shutdown_signal`), which drains readiness and closes every client with `going_away`. Any startup failure (bad config, a data directory another server holds or damaged data, unsupported durability, port in use) is logged and the process exits with a failure status. Also has a `client` subcommand, a demo built on `popsub_sdk` that connects, subscribes, publishes with confirmation and prints the delivered message, `users`, which manages accounts on a running server through `Client::admin`, and `topics`, which lists, shows and purges topics the same way.

## Structure

- `src/main.rs`: `Command` enum (`server`, `client`, `users`, `topics`), `run_server`, `open_storage`, `open_users`, `run_client`.
- `src/telemetry.rs`: `Telemetry`. `init` sets up console logging and, with `[telemetry] enabled`, the OTLP trace, metric and log exporters, and installs the meter provider globally; `register_gauges` adds the gauges read from `SharedBroker::stats`; `shutdown` flushes.
- `src/topics.rs`: the `topics` subcommands (`list`, `show`, `purge`), which reuse `users::Connection` and `users::connect`.
- `src/tests.rs`: unit tests for the startup decisions, such as `effective_durability`.
- `src/users.rs`: the `users` subcommands (`list`, `show`, `add`, `grant`, `revoke`, `role`, `passwd`, `disable`, `enable`, `delete`), parsing `--topic PATTERN[:ACCESS]`, and printing accounts.

## Rules

- Keep this crate thin. Wiring only: no protocol parsing, routing or storage logic here. Those belong in transport, broker and storage. `open_storage` only maps the configured backend name to a constructor.
- `client` and `users` take `--ca-cert` to trust a PEM certificate for `wss://` (`trust`).
- `run_client` uses `popsub_sdk`, so protocol changes reach it through the SDK. Don't write WebSocket or JSON frame code here.
- The client defaults to `admin` / `password`, which match only the default settings; `--username` and `--password` override them. It is a demo; the client library is `popsub_sdk`.
- `server` loads its settings before logging starts (`start_server`), because `[telemetry]` decides how logging is set up; a settings error goes to standard error. `client` logs at `info` and `users` at `warn`. `RUST_LOG` is not read yet.
- `Telemetry::init` must run before anything records a metric: `popsub_utils::metrics` binds its instruments to the global provider on first use, and ones made earlier stay no-ops.
- Keep `opentelemetry-otlp` without its TLS features: `reqwest-rustls` brings in aws-lc, a C build. TLS to a vendor belongs in a collector.
- Passwords for `users` never come from a positional argument. The admin's comes from `--password`, `POPSUB_PASSWORD` or a prompt; a new one from `POPSUB_NEW_PASSWORD` or a prompt that asks twice.
- `users` connects with `max_attempts: Some(0)`, so a server that is down fails at once instead of retrying.

## Run

- `make server`, `make client`, or `make demo PORT=8090` for the full round trip.
