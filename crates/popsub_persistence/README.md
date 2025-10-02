# popsub_persistence

Persistence layer using `sled` to store and replay messages per-topic.

Exports:

- `Persistence` — create with `Persistence::new(path, ttl_seconds, max_messages)`
- `StoredMessage` — serialized message
