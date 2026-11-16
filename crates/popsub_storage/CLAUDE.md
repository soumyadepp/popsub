# popsub_storage

The storage layer from `docs/STORAGE.md`: per-topic append-only logs of numbered records, durable subscription cursors, the traits a backend implements, the in-memory backend, and the conformance suite. Nothing in the server uses it yet; the broker moves onto it in rollout step 2.

## Files

- `src/record.rs`: `NewRecord` (what a publisher sends), `Record` (what the log stores, with `seq`), `Subscription` (a session's cursor), `LogBounds`.
- `src/storage.rs`: the `MessageLog`, `SubscriptionStore` and `Storage` traits, `Durability`, `Capabilities`, `Retention`.
- `src/error.rs`: `StorageError` (`Unavailable`, `Corrupt`, `Unsupported`, `Other`) and `Result`.
- `src/memory.rs`: `MemoryStorage`, the reference backend. Two mutexes, `Durability::None` only, not persistent.
- `src/conformance.rs`: the `Fixture` trait, one public async check per rule, and the `conformance_tests!` macro that turns them into tests. Compiled for this crate's tests and behind the `conformance` feature.

## Invariants

- `seq` starts at 1, increases by exactly 1 per topic and is never reused, even after every record is trimmed. `LogBounds` keeps `last_seq` for that reason.
- Publish times never decrease along a log; `append` raises a late record's time to the previous one's. `seq_at` relies on it.
- `append` returns the record as stored. A repeated `(topic, publisher, message_id)` returns the original record while it is retained; callers spot a duplicate by its `seq` being at or below the `last_seq` they saw before. Once trimmed, the same id appends a new record.
- Cursors only move forward. Acks for subscriptions that don't exist are ignored, not errors.
- Trimming always goes through `Retention::must_trim`, so every backend applies the same rules.
- A durability level above `capabilities().max_durability` is refused with `Unsupported`, never silently downgraded.

## Adding a backend

Implement `MessageLog`, `SubscriptionStore` and `Storage`, write a `Fixture` that opens the backend at a fresh location (and reopens the same one when called again), and add `popsub_storage::conformance_tests!(MyFixture::new());` in a test module. Every check must pass. When you add a rule to the suite, add its name to the list in the macro too, or it won't run.

## Tests

- `make test-crate CRATE=popsub_storage`. `src/tests.rs` runs the whole suite against `MemoryStorage` plus unit tests for the types.
