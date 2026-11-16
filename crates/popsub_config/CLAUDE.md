# popsub_config

Loads `Settings` for the server and broker.

## How loading works

1. Read `config/default.toml` relative to the working directory, if it exists.
2. Overlay environment variables. One-word keys take one underscore after the section (`SERVER_PORT`); keys containing an underscore take two (`SERVER__JWT_SECRET`, `BROKER__MAX_RETRIES`). Both forms are read.
3. Deserialise into `PartialSettings`, where every field is optional, then fill gaps from `Settings::default()`.

## Adding a setting

Update all of these together:

- the field in `ServerSettings` or `BrokerSettings`
- the matching `Option` field in the `Partial` struct
- `Settings::default()`
- the merge in `load_config` in `src/lib.rs`
- `config/default.toml` at the repo root
- the table in `docs/DESIGN.md` section 9

## Gotchas

- `jwt_secret` defaults to `default_secret` here but `a_very_secret_key` in `config/default.toml`. `Settings::insecure_defaults` reports either, and the binary refuses them on non-loopback hosts.
- `server.allow_registration` (default false) and `server.registration_role` (`none`, `readonly` or `full`, default `none`) are passed to the auth crate by the transport.
- `load_config_with_env` takes the environment as a map so tests don't touch process variables.
- `max_connections` is loaded but nothing reads it yet. `message_ttl_secs` sets the message store's TTL.
- Because the file path is relative, tests that load config change the working directory to a temp dir and restore it afterwards. Tests that do this can't run in parallel with each other safely.
- This crate uses the `config` crate at 0.15 while `popsub_utils` uses 0.13. Align them before relying on `PopSubError::Config`.
