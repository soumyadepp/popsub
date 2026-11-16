# popsub (binary)

The CLI. Loads config, refuses to start on a non-loopback host while published default secrets are set (warns on localhost), opens the `[storage]` backend (`open_storage`: `FileStorage` in `storage.path`, or `MemoryStorage`), builds a `SharedBroker` (which refuses a durability the backend can't provide), spawns its retry and retention loops and runs `Server::from_settings(...).listen(...)` until Ctrl+C. Any startup failure (bad config, a data directory another server holds or damaged data, unsupported durability, port in use) is logged and the process exits with a failure status. Also has a `client` subcommand, a demo built on `popsub_sdk` that connects, subscribes, publishes with confirmation and prints the delivered message.

## Structure

- `src/main.rs`: `Command` enum (`server`, `client --url`), `run_server`, `open_storage`, `run_client`.

## Rules

- Keep this crate thin. Wiring only: no protocol parsing, routing or storage logic here. Those belong in transport, broker and storage. `open_storage` only maps the configured backend name to a constructor.
- `run_client` uses `popsub_sdk`, so protocol changes reach it through the SDK. Don't write WebSocket or JSON frame code here.
- The client defaults to `admin` / `password`, which match only the default settings; `--username` and `--password` override them. It is a demo; the client library is `popsub_sdk`.
- Logging is initialised with a fixed `info` level. `RUST_LOG` is not read yet.

## Run

- `make server`, `make client`, or `make demo PORT=8090` for the full round trip.
