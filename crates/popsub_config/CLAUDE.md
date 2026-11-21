# popsub_config

Loads `Settings` for the server and broker.

## How loading works

1. Read `config/default.toml` relative to the working directory, if it exists.
2. Overlay environment variables. One-word keys take one underscore after the section (`SERVER_PORT`); keys containing an underscore take two (`SERVER__JWT_SECRET`, `BROKER__MAX_RETRIES`). Both forms are read.
3. Deserialise straight into `Settings`. Every settings struct except `TopicRetentionSettings` is `#[serde(default)]`, so anything neither source sets comes from that struct's `Default`. A `TopicRetentionSettings` entry must name its `pattern`; its limits are optional. A value of the wrong type is an error, not a silent fallback.

`load_config()` reads `config/default.toml` and the process environment. `load_config_from(path, env)` takes both explicitly, which is what tests use.

## Adding a setting

Update all of these together:

- the field in `ServerSettings`, `BrokerSettings`, `StorageSettings`, `RetentionSettings`, `LimitSettings`, `TelemetrySettings` or `TopicRetentionSettings`, with a one-line docstring
- that struct's `Default` impl (`TopicRetentionSettings` has none; give a new field there `#[serde(default)]`)
- `config/default.toml` at the repo root
- the table in `docs/DESIGN.md` section 9

## Gotchas

- `jwt_secret` defaults to `default_secret` here but `a_very_secret_key` in `config/default.toml`. `Settings::insecure_defaults` reports either, and the binary refuses them on non-loopback hosts.
- `server.allow_registration` (default false) and `server.registration_role` (`none`, `readonly` or `full`, default `none`) are passed to the auth crate by the transport.
- Tests pass `Some(HashMap::new())` (or their own map) as the environment so variables set on the machine running the tests can't leak in.
- `broker.max_connections` is enforced by the transport through `SharedBroker::max_connections`; `0` means no limit.
- `[limits]` (`LimitSettings`) holds the rate limits; the transport builds its limiters from it with `RateLimiters::from_settings`, which raises values below 1 to 1.
- `server.proxy_protocol` only takes effect with `server.trusted_proxies`; the binary refuses to start with it on and the list empty.
- `server.trusted_proxies` reads either a list (TOML) or one comma-separated string (the environment) through `list_or_commas`. Don't switch the environment source to `try_parsing` to get lists: it would turn secrets such as `1e5` into numbers.
- `server.heartbeat_secs` and `server.idle_timeout_secs` set the transport's `Heartbeat`; values below 1 are treated as 1. `server.max_payload_bytes` sets the publish payload limit and, through it, the WebSocket frame limit. `server.outbound_queue_bytes` is each connection's hard outgoing queue limit; the soft limit is derived from it.
- `broker.max_inflight` sizes each subscription's window of unacknowledged QoS 1 records; values below 1 behave as 1. `storage.session_expiry_secs` is how long a durable session outlives its last connection.
- `[storage]` defaults to the `file` backend in `pubsub_data` with `buffered` durability. It uses typed enums: `backend` is a `StorageBackend` and `durability` is `popsub_storage::Durability`, so an unknown name is a load error. Retention limits are plain numbers (0 keeps nothing) and `RetentionSettings::to_retention` turns them into the policy the backends apply.
- `storage.topic_retention` is a list of `TopicRetentionSettings`; `StorageSettings::topic_retention_rules` validates the patterns and fills missing limits from `storage.retention`. It can only be set in the config file: the environment source can't express a list of tables.
- Nested keys from the environment use a double underscore at every level: `STORAGE__RETENTION__MAX_MESSAGES`.
- This crate depends on `popsub_storage` for `Durability` and `Retention`. Storage must never depend back on config.
- `popsub_utils` uses the same `config` version (0.15), so `PopSubError::Config` can wrap loader errors. Bump both together.
