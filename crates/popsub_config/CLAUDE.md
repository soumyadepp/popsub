# popsub_config

Loads `Settings` for the server and broker.

## How loading works

1. Read `config/default.toml` relative to the working directory, if it exists.
2. Overlay environment variables. One-word keys take one underscore after the section (`SERVER_PORT`); keys containing an underscore take two (`SERVER__JWT_SECRET`, `BROKER__MAX_RETRIES`). Both forms are read.
3. Deserialise straight into `Settings`. Every settings struct is `#[serde(default)]`, so anything neither source sets comes from that struct's `Default`. A value of the wrong type is an error, not a silent fallback.

`load_config()` reads `config/default.toml` and the process environment. `load_config_from(path, env)` takes both explicitly, which is what tests use.

## Adding a setting

Update all of these together:

- the field in `ServerSettings`, `BrokerSettings`, `StorageSettings` or `RetentionSettings`, with a one-line docstring
- that struct's `Default` impl
- `config/default.toml` at the repo root
- the table in `docs/DESIGN.md` section 9

## Gotchas

- `jwt_secret` defaults to `default_secret` here but `a_very_secret_key` in `config/default.toml`. `Settings::insecure_defaults` reports either, and the binary refuses them on non-loopback hosts.
- `server.allow_registration` (default false) and `server.registration_role` (`none`, `readonly` or `full`, default `none`) are passed to the auth crate by the transport.
- Tests pass `Some(HashMap::new())` (or their own map) as the environment so variables set on the machine running the tests can't leak in.
- `max_connections` is loaded but nothing reads it yet.
- `server.heartbeat_secs` and `server.idle_timeout_secs` set the transport's `Heartbeat`; values below 1 are treated as 1.
- `broker.max_inflight` sizes each subscription's window of unacknowledged QoS 1 records; values below 1 behave as 1. `storage.session_expiry_secs` is how long a durable session outlives its last connection.
- `[storage]` defaults to the `file` backend in `pubsub_data` with `buffered` durability. It uses typed enums: `backend` is a `StorageBackend` and `durability` is `popsub_storage::Durability`, so an unknown name is a load error. Retention limits are plain numbers (0 keeps nothing) and `RetentionSettings::to_retention` turns them into the policy the backends apply.
- Nested keys from the environment use a double underscore at every level: `STORAGE__RETENTION__MAX_MESSAGES`.
- This crate depends on `popsub_storage` for `Durability` and `Retention`. Storage must never depend back on config.
- `popsub_utils` uses the same `config` version (0.15), so `PopSubError::Config` can wrap loader errors. Bump both together.
