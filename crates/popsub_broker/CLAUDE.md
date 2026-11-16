# popsub_broker

Topic writers, the broker engine, replay, per-subscriber acknowledgement and durable sessions. Every published record goes into its topic's log in a `popsub_storage::Storage` backend and is then delivered to subscribers as a tagged `message` frame with its `seq`.

## Files

- `src/shared.rs`: `SharedBroker`, the cloneable handle everything uses. `with(|broker| ...)` locks the broker (through the crate-private `BrokerCell`, the only lock path, which recovers from poisoning). `publish`, `subscribe`, `unsubscribe`, `ack`, `attach_session` and `forget_subscription` are `async`; publishes, subscribes and catch-ups go to the topic's writer. `cleanup_client` removes a client and touches its session in the background. Background loops: `run_retry_loop` (1 s), `run_retention_loop` (60 s), `run_session_expiry_loop` (60 s). `session_key` builds the store key from username and session id. `close_all` and `close_user` close every connection, or one user's, with a close code. `SharedBroker::new` refuses a durability the backend can't provide.
- `src/health.rs`: `StorageHealth`, every writer's append outcomes, ordered by a counter, for `SharedBroker::storage_failing`. Failures count only for a window, so a server held out of rotation can become ready again.
- `src/writer.rs`: `TopicWriter`, one task per topic, started on first use. Handles `Publish` (append, deliver, store cursors, trim), `Subscribe` (store the subscription, add it, send `subscribed` and `truncated`, catch up) and `CatchUp` (read the log from a subscription's `next_seq` and send while its window has room) one at a time.
- `src/engine.rs`: `Broker`, synchronous: clients, subscriptions, `deliver`, `send_records`, `skip_to`, `ack`, `retry_overdue` (resends per subscription, stalls after `max_retries`), session lookups, and `subscriptions_of_user` for ending subscriptions after an admin change.
- `src/topic.rs`: `Topic` and each subscription's `DeliveryState` (`next_seq`, the in-flight window, stall, and the derived `cursor`), plus `CursorUpdate`.
- `src/replay.rs`: `plan`, which turns a `StartFrom` and the log's bounds into a starting `seq` and whether to report a truncation. Pure, unit tested.
- `src/frame.rs`: `delivery` (a stored record as a `message` frame) and `to_ws`. The frame types themselves come from `popsub_protocol`.
- `src/tests.rs`: unit tests with `mpsc` channels as fake clients and `MemoryStorage` as the log and session store.

## Contract

- Storage is only touched outside the broker lock. The lock is only taken through `with`, whose closure can't await.
- Every publish, subscribe and catch-up for a topic goes through that topic's writer. That is what gives seq-ordered delivery and gap-free replay; `concurrent_publishers_are_delivered_in_seq_order` and `replay_and_live_delivery_never_overlap_or_gap` fail without it.
- A subscription is only sent the exact `seq` it expects next. Anything it misses is read back from the log by catch-up, never queued in memory.
- Deliveries and catch-up check the client's `Outbox::has_room` and skip a busy client. `deliver` and `send_records` report it once (`awaiting_room`), and the writer's `catch_up_when_ready` waits on `Outbox::ready` and then queues a `CatchUp`. Without that wait a busy subscriber stalls; `a_busy_subscriber_catches_up_when_its_queue_drains` guards it.
- ACKs are per subscription: only a record in that connection's window is accepted. `acks_are_per_subscriber` and `retries_go_only_to_the_subscriber_that_has_not_acked` guard this.
- The cursor is derived (one below the oldest in-flight record, or below `next_seq`); never store it separately in memory.
- Sessions are keyed by `session_key(username, session_id)`, so one user can't reach another's.
- The broker writes `popsub_protocol::ServerFrame`s and must not depend on transport types. `subscribed` carries the subscribe request's `id`; everything else it sends is an event with none.

## Current behaviour to keep in mind

- `Resume` (the default `from`) becomes one past the stored cursor for a durable session with a stored subscription to the topic, and `New` otherwise.
- QoS 0 records count as done once sent and move the cursor. While a window is full, QoS 0 waits too.
- A stalled subscription gets nothing, not even retries, until it ACKs something (which resets the remaining records' retries) or resubscribes.
- Resubscribing replaces the subscription and drops its window; for a session, `resume` replays from the stored cursor.
- Attaching a session held by another connection sends that connection a close frame and removes it.
- A cursor that fails to store is logged; the worst case is resending acknowledged records after a reconnect.
- A long catch-up holds up publishes to that topic until it finishes.

## When changing delivery logic

- Write the failing test first in `src/tests.rs`: several subscribers, a missing ACK, a reconnect, concurrent publishers.
- Update the semantics table in `docs/DESIGN.md` section 6 in the same change.
