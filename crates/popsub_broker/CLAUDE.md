# popsub_broker

The engine: topics and their subscriber sets, publishing, QoS 1 pending table and retry loop, and replay on subscribe.

## Files

- `src/engine.rs`: `Broker`, `PendingMessage`, and `retry_overdue`, one pass of QoS 1 retries that tests can call with a chosen time.
- `src/shared.rs`: `SharedBroker`, the cloneable handle every task uses. `with(|broker| ...)` is the only way to lock the broker; it recovers from poisoning instead of panicking. `run_retry_loop` calls `retry_overdue` every second.
- `src/topic.rs`: `Topic`, a name and a `HashSet` of subscriber ids.
- `src/message.rs`: `Message`, the internal and wire form of a published message.
- `src/tests.rs`: unit tests using `tokio::sync::mpsc` channels as fake clients.

## Contract

- The API is synchronous and is shared through `SharedBroker`. Never add a way to hold its lock across an `.await`.
- The broker pushes `tungstenite` messages into each `Client`'s channel. It must not depend on transport types or know about the JSON protocol beyond serialising `Message`.

## Current behaviour to keep in mind

- `pending_acks` is keyed by `message_id` only. One ACK from any subscriber clears the message for everyone, and retries go to every subscriber. This is the main gap in the at-least-once claim (`docs/DESIGN.md` section 6).
- `subscribe` replays the full stored history for the topic, as QoS 0 with new ids, every time.
- `publish` writes to `sled` while holding the lock.
- Topics are only created by `subscribe`. Publishing to an unknown topic still stores the message and logs "Topic not found".
- `Broker::new` uses `Persistence::default()`, which panics if the store can't be opened. The binary avoids it by opening the store itself and calling `new_with_persistence`; prefer that everywhere.
- `Broker::default` branches on `cfg!(test)` and uses `tempfile`, which is why `tempfile` is a runtime dependency. Prefer `new_with_persistence` with a temp dir in tests.
- `Broker::ACK_TIMEOUT` is only used by tests. `retry_overdue` reads `settings.ack_timeout_ms`, so the two can drift if the default changes.

## When changing delivery logic

- Write the failing test first in `src/tests.rs`: several subscribers, a missing ACK, a reconnect.
- Update the semantics table in `docs/DESIGN.md` section 6 in the same change.
