# popsub (binary)

The CLI. Loads config, refuses to start on a non-loopback host while published default secrets are set (warns on localhost), opens the `[storage]` backend (`open_storage`: `FileStorage` in `storage.path`, or `MemoryStorage`) and the matching user store (`open_users`: `users.json` in `storage.path`, or in memory), builds a `SharedBroker` (which refuses a durability the backend can't provide), spawns its retry and retention loops and runs `Server::from_settings(...).listen(...)` until Ctrl+C. Any startup failure (bad config, a data directory another server holds or damaged data, unsupported durability, port in use) is logged and the process exits with a failure status. Also has a `client` subcommand, a demo built on `popsub_sdk` that connects, subscribes, publishes with confirmation and prints the delivered message, and `users`, which manages accounts on a running server through `Client::admin`.

## Structure

- `src/main.rs`: `Command` enum (`server`, `client`, `users`), `run_server`, `open_storage`, `open_users`, `run_client`.
- `src/users.rs`: the `users` subcommands (`list`, `show`, `add`, `grant`, `revoke`, `role`, `passwd`, `disable`, `enable`, `delete`), parsing `--topic PATTERN[:ACCESS]`, and printing accounts.

## Rules

- Keep this crate thin. Wiring only: no protocol parsing, routing or storage logic here. Those belong in transport, broker and storage. `open_storage` only maps the configured backend name to a constructor.
- `run_client` uses `popsub_sdk`, so protocol changes reach it through the SDK. Don't write WebSocket or JSON frame code here.
- The client defaults to `admin` / `password`, which match only the default settings; `--username` and `--password` override them. It is a demo; the client library is `popsub_sdk`.
- Logging is initialised at `info`, or `warn` for `users` so its output stays readable. `RUST_LOG` is not read yet.
- Passwords for `users` never come from a positional argument. The admin's comes from `--password`, `POPSUB_PASSWORD` or a prompt; a new one from `POPSUB_NEW_PASSWORD` or a prompt that asks twice.
- `users` connects with `max_attempts: Some(0)`, so a server that is down fails at once instead of retrying.

## Run

- `make server`, `make client`, or `make demo PORT=8090` for the full round trip.
