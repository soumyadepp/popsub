# popsub_storage

The storage layer from `docs/STORAGE.md`: per-topic append-only logs of numbered records, durable subscription cursors, the traits a backend implements, the `file` and `memory` backends, and the conformance suite. The broker's topic writers use it for every publish, replay and cursor, and `popsub_config` uses `Durability` and `Retention` for the `[storage]` settings.

## Files

- `src/record.rs`: `NewRecord` (what a publisher sends), `Record` (what the log stores, with `seq`), `Subscription` (a session's cursor), `LogBounds`, and `PayloadEncoding`, the optional `encoding` both records carry beside the payload string (never decoded; records stored before the field existed read back as `None`).
- `src/storage.rs`: the `MessageLog`, `SubscriptionStore` and `Storage` traits, `Durability`, `Capabilities`, `Retention`.
- `src/error.rs`: `StorageError` (`Unavailable`, `Corrupt`, `Unsupported`, `Other`) and `Result`.
- `src/index.rs`: `LogIndex<T>`, the per-topic numbering, retry detection, publish-time and retention rules. Both backends use it; `T` is the whole record in memory and its disk location in the file backend.
- `src/sessions.rs`: `SessionTable`, the session and cursor rules, used by both backends.
- `src/memory.rs`: `MemoryStorage`. `Durability::None` only, not persistent.
- `src/file/`: `FileStorage`, the default backend.
  - `mod.rs`: the store, the `LOCK` file, topic directory names (`t` + hex), and running file I/O on the blocking pool.
  - `topic.rs`: one topic's segment files, `head.json`, recovery and trimming.
  - `sessions.rs`: the session write-ahead log and its compaction.
  - `codec.rs`: the `length | crc32 | body` frame.
  - `tests.rs`: the suite with 256-byte segments, and the recovery and damage tests.
- `src/faulty.rs`: `FaultyStorage`, a wrapper that fails appends while switched on and passes everything else through. For tests of what breaks when storage does.
- `src/tests.rs`: tests for the storage types, and the conformance suite run against `MemoryStorage`.
- `tests/crash.rs`: the crash test, in its own test binary (see below).
- `src/conformance.rs`: the `Fixture` trait, one public async check per rule, and the `conformance_tests!` macro. Compiled for this crate's tests and behind the `conformance` feature.

## Invariants

- `seq` starts at 1, increases by exactly 1 per topic and is never reused, even after every record is trimmed. `LogBounds` keeps `last_seq`; the file backend persists it in `head.json`.
- Publish times never decrease along a log; `append` raises a late record's time to the previous one's. `seq_at` relies on it.
- `append` returns the record as stored. A repeated `(topic, publisher, message_id)` returns the original record while it is retained. Once trimmed, the same id appends a new record.
- Cursors only move forward. Acks for subscriptions that don't exist are ignored, not errors.
- Numbering, retry and retention rules live in `LogIndex`, session rules in `SessionTable`. Don't reimplement them in a backend.
- A durability level above `capabilities().max_durability` is refused with `Unsupported`, never silently downgraded.
- File backend: the head is written before segments are deleted; a bad frame is only truncated at the end of the newest segment, anything else is `Corrupt`; nothing confirmed as `synced` is acknowledged before `sync_data` returns.
- File backend: a trim that doesn't empty a segment only changes the index (`trim_index`) and marks the head stale; `persist_trim` writes it when a segment or the whole log empties, and `Drop` writes a stale one. So after a crash up to a segment of trimmed records is back until the next trim. A purge always persists.

## Adding a backend

Implement `MessageLog`, `SubscriptionStore` and `Storage`, write a `Fixture` that opens the backend at a fresh location (and reopens the same one when called again), and add `popsub_storage::conformance_tests!(MyFixture::new());` in a test module. Every check must pass. When you add a rule to the suite, add its name to the list in the macro too, or it won't run.

## Gotchas

- `FileStorage` scans every segment on open and keeps each retained record's location in memory. Startup time and memory grow with retained data.
- `MessageLog::append_batch` is the group commit: one write, and with `synced` one flush, for the whole batch. `append` is a batch of one. A backend that doesn't override it appends one record at a time.
- A record appended with an empty `message_id` gets a random one (`Record::from_new`) and isn't in the retry index, so it can never be a duplicate.
- `Record::write_json` and `push_json_str` write a record's JSON by hand because `serde_json` was the largest cost per message. `a_record_writes_the_same_json_as_serde` holds them equal to the derived `Serialize`; add a field to both.
- The crash test re-runs its own test binary as a child (`crash_child`, gated by `POPSUB_CRASH_CHILD_DIR`). It must stay in a separate test binary: a spawned child briefly holds copies of the parent's file descriptors, including a store's lock file, and a test that reopens a store in that window finds the directory locked. CI hit exactly that when the crash test lived in `src/file/tests.rs`. Never spawn processes from the lib tests.

## Tests

- `make test-crate CRATE=popsub_storage`.
