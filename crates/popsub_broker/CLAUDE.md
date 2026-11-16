# popsub_broker

Topic writers, the broker engine, replay and QoS 1 retries. Every published record goes into its topic's log in a `popsub_storage::Storage` backend and is then delivered to subscribers as a tagged `message` frame with its `seq`.

## Files

- `src/shared.rs`: `SharedBroker`, the cloneable handle everything uses. `with(|broker| ...)` locks the broker (through the crate-private `BrokerCell`, the only lock path, which recovers from poisoning). `publish` and `subscribe` are `async` and go to the topic's writer. `run_retry_loop` (every second) and `run_retention_loop` (every minute) are spawned by the binary. `LogSettings` holds durability and retention. `SharedBroker::new` refuses a durability the backend can't provide.
- `src/writer.rs`: `TopicWriter`, one task per topic, started on first use. Handles `Publish` (append, deliver if new, trim) and `Subscribe` (replay, then add the subscriber) one at a time. `Published` and `Subscribed` are their outcomes.
- `src/replay.rs`: `plan`, which turns a `StartFrom` and the log's bounds into a starting `seq` and whether to report a truncation. Pure, unit tested.
- `src/engine.rs`: `Broker`, synchronous: clients, subscriber sets, `deliver` (fan-out plus `pending_acks`), ACKs, `retry_overdue`.
- `src/frame.rs`: `Delivery`, `Truncated`, `TopicFrame` (what the broker writes to channels) and `StartFrom` (the `from` of a subscribe).
- `src/topic.rs`: `Topic`, a name and a set of subscriber ids.
- `src/tests.rs`: unit tests with `mpsc` channels as fake clients and `MemoryStorage` as the log.

## Contract

- Storage is only touched in a topic writer, outside the broker lock. The lock is only taken through `with`, whose closure can't await.
- Every publish and subscribe for a topic goes through that topic's writer. That is what gives seq-ordered delivery and gap-free replay; `concurrent_publishers_are_delivered_in_seq_order` and `replay_and_live_delivery_never_overlap_or_gap` fail without it.
- Only the writer calls `Broker::deliver`, once per new record, in `seq` order.
- The broker serialises its own frames and must not depend on transport types. The transport's `ServerMessage` wraps `Delivery` and `Truncated`; a transport test checks they still match.

## Current behaviour to keep in mind

- `pending_acks` is keyed by `message_id` only. One ACK from any subscriber clears the record for everyone, and retries go to every subscriber. Replayed records are never added to it. Step 3 of `docs/STORAGE.md` replaces it with per-subscription windows.
- A subscribe without `from` replays nothing (`StartFrom::New`).
- A duplicate publish (same publisher and message id, still retained) returns the original `seq` and is not delivered again.
- If storage fails during a subscribe, the client is not subscribed.
- A long replay holds up publishes to that topic until it finishes.
- Writer tasks hold the broker and storage but not the `SharedBroker`, so they stop when every handle is dropped.

## When changing delivery logic

- Write the failing test first in `src/tests.rs`: several subscribers, a missing ACK, a reconnect, concurrent publishers.
- Update the semantics table in `docs/DESIGN.md` section 6 in the same change.
