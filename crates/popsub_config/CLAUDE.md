# popsub_config

Loads `Settings` for the server and broker.

## How loading works

1. Read `config/default.toml` relative to the working directory, if it exists.
2. Overlay environment variables, using `_` as the separator: `SERVER_PORT`, `BROKER_MAX_RETRIES`.
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

- `jwt_secret` defaults to `default_secret` here but `a_very_secret_key` in `config/default.toml`. Neither should be used outside development.
- `max_connections` and `message_ttl_secs` are loaded but nothing reads them yet.
- Because the file path is relative, tests that load config change the working directory to a temp dir and restore it afterwards. Tests that do this can't run in parallel with each other safely.
- This crate uses the `config` crate at 0.15 while `popsub_utils` uses 0.13. Align them before relying on `PopSubError::Config`.
