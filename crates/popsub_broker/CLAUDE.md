# popsub_broker

Topic writers, the broker engine, replay, per-subscriber acknowledgement and durable sessions. Every published record goes into its topic's log in a `popsub_storage::Storage` backend and is then delivered to subscribers as a tagged `message` frame with its `seq`.

## Files

- `src/shared.rs`: `SharedBroker`, the cloneable handle everything uses. `with(|broker| ...)` locks the broker (through the crate-private `BrokerCell`, the only lock path; a `parking_lot` mutex, so a panic under it doesn't poison it). `BrokerCell::with_fair` is the same but hands the lock to the longest waiter after a hold of 20 microseconds or more; topic writers deliver through it. `publish`, `subscribe`, `unsubscribe`, `ack`, `attach_session` and `forget_subscription` are `async`; publishes, subscribes and catch-ups go to the topic's writer. `cleanup_client` removes a client and touches its session in the background. Background loops: `run_retry_loop` (1 s), `run_retention_loop` (60 s), `run_session_expiry_loop` (60 s). `session_key` builds the store key from username and session id. `close_all` and `close_user` close every connection, or one user's, with a close code. `SharedBroker::new` refuses a durability the backend can't provide. `topic_infos`, `topic_info` and `purge_topic` back the topic admin requests; purge trims everything and keeps `last_seq`, never deleting the log. `with_topic_retention` sets per-pattern policies and `retention_for` picks a topic's; a writer gets its topic's policy when it starts.
- `src/health.rs`: `StorageHealth`, every writer's append outcomes, ordered by a counter, for `SharedBroker::storage_failing`. Failures count only for a window, so a server held out of rotation can become ready again.
- `src/writer.rs`: `TopicWriter`, one task per topic, started on first use, on the runtime given to `SharedBroker::with_writer_runtime` when there is one. Handles `Publish` (append, deliver, store cursors, trim), `Subscribe` (store the subscription, add it, send `subscribed` and `truncated`, catch up) and `CatchUp` (read the log from a subscription's `next_seq` and send while its window has room) in order. Publishes waiting next to each other in the queue are one `publish_batch`, up to `PUBLISH_BATCH` records or `PUBLISH_BATCH_BYTES` of payload: one `append_batch`, then `Broker::deliver_batch` in holds of at most `LOCK_HOLD`.
- `src/sink.rs`: `PublishSink`, one per connection. The transport queues a publish through it (`SharedBroker::publish_from`) and doesn't wait; the writer answers the client through the sink (`published`, or `storage_unavailable`). It counts the connection's unfinished publishes (`is_full`, `has_room`, `drained`) and remembers the last topic's writer.
- `src/engine.rs`: `Broker`, synchronous: clients, subscriptions, `deliver_batch` (and `deliver`, the same for one record), `send_records`, `skip_to`, `ack`, `retry_overdue` (resends per subscription, stalls after `max_retries`), session lookups, and `subscriptions_of_user` for ending subscriptions after an admin change.
- `src/topic.rs`: `Topic` and each subscription's `DeliveryState` (`next_seq`, the in-flight window, stall, and the derived `cursor`), plus `CursorUpdate`.
- `src/replay.rs`: `plan`, which turns a `StartFrom` and the log's bounds into a starting `seq` and whether to report a truncation. Pure, unit tested.
- `src/frame.rs`: `delivery` (a stored record as a `message` frame), `delivery_ws` (the same frame written straight to text without building the struct; a test holds the two equal), `to_ws`, and the public `close_message` (a close frame with a `CloseCode`). The frame types themselves come from `popsub_protocol`.
- `src/tests.rs`: unit tests with `mpsc` channels as fake clients and `MemoryStorage` as the log and session store.

- Wildcards: `Broker::add_pattern`/`remove_pattern`/`pattern_holders` track each client's patterns and which topics it joined through them (`PatternState`); `SharedBroker::join` subscribes without an id and marks the topic joined; `publish` calls `join_pattern_holders` after a topic's first record, which is what makes a concurrent pattern subscribe and first publish safe (the pattern is registered before topics are listed, and holders are read after the append). `set_authorizer` is how the transport's permission check gets in without the broker depending on auth.

- Shared subscriptions: `Topic::groups` maps a group's subscriber id to its `Group` (members, round-robin position, `owners`: which member each in-flight `seq` was sent to). The group's `DeliveryState` sits in `Topic::subscribers` under the same id, with that id as its session key so its cursor is stored. `add_group_member` joins (and hands waiting records to the new member), `leave_groups` hands a leaving member's records on, `ack` finds the group through `owners`, and `retry_overdue` resolves the client per record. `TopicWriter::subscribe_group` decides where a new group starts; `expire_sessions` touches active groups so their cursors don't expire.

## Contract

- Storage is only touched outside the broker lock. The lock is only taken through `with`, whose closure can't await.
- Every publish, subscribe and catch-up for a topic goes through that topic's writer. That is what gives seq-ordered delivery and gap-free replay; `concurrent_publishers_are_delivered_in_seq_order` and `replay_and_live_delivery_never_overlap_or_gap` fail without it.
- A subscription is only sent the exact `seq` it expects next. Anything it misses is read back from the log by catch-up, never queued in memory. `deliver_batch` relies on this: it gives the broker lock up part way through a topic's subscribers and, if one was added or removed meanwhile (`Topic::epoch`), starts the pass again, which is safe only because a subscription that already took the batch takes nothing twice. Change `Topic::subscribers` only through code that bumps `epoch`.
- Deliveries and catch-up check the client's `Outbox::has_room` and skip a busy client. `deliver_batch` and `send_records` report it once (`awaiting_room`), and the writer's `catch_up_when_ready` waits on `Outbox::ready` (the queue down to half its soft limit) and then queues a `CatchUp`. A catch-up that ends with the queue full again calls `mark_awaiting_room`, or nothing would start the next one. Without that wait a busy subscriber stalls; `a_busy_subscriber_catches_up_when_its_queue_drains` guards it.
- ACKs are per subscription: only a record in that connection's window is accepted. `acks_are_per_subscriber` and `retries_go_only_to_the_subscriber_that_has_not_acked` guard this.
- The cursor is derived (one below the oldest in-flight record, or below `next_seq`); never store it separately in memory.
- Sessions are keyed by `session_key(username, session_id)`, so one user can't reach another's.
- The broker writes `popsub_protocol::ServerFrame`s and must not depend on transport types. `subscribed` carries the subscribe request's `id`; everything else it sends is an event with none.

## Current behaviour to keep in mind

- A client's publish is not awaited by its connection. Replies to publishes come from the writer, so they can arrive after the reply to a later request that was refused straight away. A request other than publish, ack or ping waits for `PublishSink::drained` first.
- A record published without a `message_id` gets its id from storage (`Record::from_new`) and is never treated as a retry.
- The `published` log line is at `debug`. At `info` it was the largest thing threads waited on.
- `Resume` (the default `from`) becomes one past the stored cursor for a durable session with a stored subscription to the topic, and `New` otherwise.
- QoS 0 records count as done once sent and move the cursor. While a window is full, QoS 0 waits too.
- With `broker.dead_letter_topic` set, `retry_overdue` drops a record that is out of retries from the subscription's window and queues it in `take_dead_letters` instead of stalling; `SharedBroker::retry_pass` then publishes the envelope to that topic, stores moved cursors and catches the subscription up. Subscriptions on the dead-letter topic itself still stall.
- A stalled subscription gets nothing, not even retries, until it ACKs something (which resets the remaining records' retries) or resubscribes.
- Resubscribing replaces the subscription and drops its window; for a session, `resume` replays from the stored cursor.
- Attaching a session held by another connection sends that connection a close frame and removes it.
- A cursor that fails to store is logged; the worst case is resending acknowledged records after a reconnect.
- A long catch-up holds up publishes to that topic until it finishes.

## When changing delivery logic

- Write the failing test first in `src/tests.rs`: several subscribers, a missing ACK, a reconnect, concurrent publishers.
- Update the semantics table in `docs/DESIGN.md` section 6 in the same change.
