# popsub (binary)

The CLI. Loads config, refuses to start on a non-loopback host while published default secrets are set (warns on localhost), opens the message store in `DATA_DIR`, builds a `SharedBroker`, spawns its retry loop and runs `Server::from_settings(...).listen(...)` until Ctrl+C. Any startup failure (bad config, locked store, port in use) is logged and the process exits with a failure status. Also has a `client` subcommand, the one demo client, which walks through login, auth, subscribe and publish using the transport's own message types.

## Structure

- `src/main.rs`: `Command` enum (`server`, `client --url`), `run_server`, `run_client`.

## Rules

- Keep this crate thin. Wiring only: no protocol parsing, routing or storage logic here. Those belong in transport, broker and persistence.
- `run_client` builds frames from `popsub_transport::ClientMessage`, so protocol changes reach it automatically. Don't hand-write JSON frames here.
- The client defaults to `admin` / `password`, which match only the default settings; `--username` and `--password` override them. It is a demo, not a client library.
- Logging is initialised with a fixed `info` level. `RUST_LOG` is not read yet.

## Run

- `make server`, `make client`, or `make demo PORT=8090` for the full round trip.
