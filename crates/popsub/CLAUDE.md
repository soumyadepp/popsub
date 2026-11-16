# popsub (binary)

The CLI. Loads config, refuses to start on a non-loopback host while published default secrets are set (warns on localhost), opens the message store in `DATA_DIR`, builds a `SharedBroker`, spawns its retry loop and runs `Server::from_settings(...).listen(...)` until Ctrl+C. Any startup failure (bad config, locked store, port in use) is logged and the process exits with a failure status. Also has a `client` subcommand and `examples/simple_client.rs` that walk through login, auth, subscribe and publish.

## Structure

- `src/main.rs`: `Command` enum (`server`, `client --url`), `run_server`, `run_client`.
- `examples/simple_client.rs`: the same flow as `run_client`, as a standalone example.

## Rules

- Keep this crate thin. Wiring only: no protocol parsing, routing or storage logic here. Those belong in transport, broker and persistence.
- `run_client` and the example duplicate each other. If you change the protocol, update both, or replace one with a call to the other.
- The demo client hard-codes `admin` and `password`. That matches the default config only. Don't treat it as a real client library.
- Logging is initialised with a fixed `info` level. `RUST_LOG` is not read yet.

## Run

- `make server`, `make client`, `make example`, or `make demo PORT=8090` for the full round trip.
