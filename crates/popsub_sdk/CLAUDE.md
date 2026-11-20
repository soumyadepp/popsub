# popsub_sdk

The Rust client library. `Client` keeps a connection to a PopSub server and handles login, token refresh, reconnection, resubscription, confirmed publishes, acknowledgements and deduplication, so applications only see `subscribe`, `publish`, `publish_confirmed`, `Subscription::next` and `Message::ack`.

## Files

- `src/client.rs`: `Client`, the cheap-to-clone handle. Every call is a `Command` sent to the driver, with a `request_timeout`.
- `src/driver.rs`: the driver task that owns the WebSocket. `run` loops connect, serve, reconnect; `connect` logs in and authenticates; `serve` reads frames and commands; `restore` resubscribes and resends after a reconnect; `deliver` deduplicates and acknowledges.
- `src/admin.rs`: `Admin`, from `Client::admin`, one method per admin request. They go through `Command::Request`, which is never queued while disconnected and never resent: a disconnect fails it with `Error::Connect`, because the server may or may not have carried it out.
- `src/config.rs`: `ClientConfig`, `Credentials`, `AckMode`, `ReconnectPolicy`.
- `src/message.rs`: `Message`, `Subscription` (unsubscribes on drop), `Event`.
- `src/error.rs`: `Error` and `is_temporary`.
- `src/tests.rs`: tests against a real server built from the workspace crates (in-memory, and file storage for the restart test).

## Invariants

- Each `(topic, seq)` reaches the application at most once per client: `deliver` drops anything below the topic's `next_expected`. A dropped duplicate is acknowledged again if it was acknowledged before (or in `AckMode::Auto`), so a lost ack can't stall the subscription.
- After a reconnect, topics the server restored for a durable session are left to it; every other topic is resubscribed from `StartFrom::Seq(next_expected)`, never from the original `from`, so an ephemeral client misses nothing still in the log.
- `connect` sends `hold_resumed` whenever there is a session. When the server answers `held`, it sends nothing on the session's topics until this client subscribes, so `unclaimed` stays empty and `restore` subscribes to every tracked resumed topic from `Resume`. The next invariant is the fallback for a server that doesn't hold.
- A topic the server restored for the session (not held) that the application hasn't subscribed to in this process is in `unclaimed`: `deliver` drops its messages without an acknowledgement, so the server keeps them. Subscribing to it (by name, or through a pattern that matches, which sends a `subscribe` by name with `Resume`) sets `Topic::claiming`, and messages are still dropped unacknowledged until that subscribe's `subscribed` arrives, because they are the server's own resume and the subscribe replays them. Never acknowledge on either path; that is how a restarted application lost messages. `restore` resubscribes a topic that is still `claiming` even when the server lists it as resumed.
- Subscribing again to a held pattern replaces it: `add_topic` moves the topics it owned to the new buffer and generation and keeps their `next_expected`.
- Confirmed publishes stay in `unconfirmed` until `published` arrives and are resent with the same request id and `message_id`, so the server stores them once.
- The driver holds only a weak handle on its own command queue (for `Message::ack`), so it stops when every `Client` and `Subscription` is dropped.
- An `unsubscribed` with no id means the server ended the subscription (an admin revoked access): the topic is dropped, ending its `Subscription`, and `Event::Unsubscribed` is sent. So every unsubscribe the driver sends carries an id; never send one without.
- `wss://` goes through `tls_connector`: rustls with `ring`, the public web roots from `webpki-roots`, plus `ClientConfig::add_root_certificates_pem`. Always pass explicit providers to rustls (`builder_with_provider`); never rely on a process-wide default, which panics if two crypto backends end up compiled in.
- A wildcard subscription is a `Pattern`; topics the server joins for it are `Topic`s with `owner` set, adopted on `subscribed` (or on a message for an untracked topic). `release` drops a pattern with its topics. `restore` resubscribes topics first, then patterns from `Seq(1)`.
- A shared subscription's `Topic` has `group` set: duplicates are found with `seen`, never `next_expected`, and `restore` rejoins it with `Resume` and the group whether or not it was "resumed".
- Close codes decide reconnection: `CloseCode::should_reconnect`, except that `TokenExpired` reconnects only with password credentials.
- A full subscription buffer blocks the driver, which stops reading the socket; the server's backpressure then paces this connection. That is deliberate.

## Gaps

- No client certificates (mutual TLS) yet.
- No metrics hooks yet; use `Client::events`.

## Tests

- `make test-crate CRATE=popsub_sdk`. The tests start a server on a free port; the restart test reopens the same `FileStorage` directory once the first server lets go of it.
