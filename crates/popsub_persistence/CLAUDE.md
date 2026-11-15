# popsub_persistence

Stores recent messages per topic in an embedded `sled` database so new subscribers can be replayed history.

## Layout

- One `sled` tree per topic, named after the topic.
- Key: `{timestamp_ms:020}_{uuid}`, so iteration is chronological.
- Value: JSON `StoredMessage { topic, payload, timestamp }`.

## Behaviour

- `store_message(topic, payload)`: inserts, then trims the oldest entries beyond `max_messages_per_topic`.
- `load_messages(topic)`: deletes entries older than `ttl_seconds`, then returns the rest in order.
- `Persistence::default()`: path `pubsub_db` (relative to the working directory), TTL 3600 seconds, cap 1000.

## Gotchas

- The message id and QoS level are not stored, so replays can't be matched to the original message or deduplicated. Adding them changes the stored format; old entries must still deserialise (use `#[serde(default)]`).
- TTL is enforced only when a topic is read, not on a timer.
- `sled` locks its directory. Two processes, or two `Persistence` values in one test, can't open the same path. Tests use `tempfile::tempdir()`.
- `sled` 0.34 is old and its successor is still alpha. Keep storage behind this crate's API so it can be swapped for an append-only log later.

## Tests

- `make test-crate CRATE=popsub_persistence`. Always use a temp directory, never `pubsub_db`.
