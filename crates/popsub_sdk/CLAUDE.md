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
- Confirmed publishes stay in `unconfirmed` until `published` arrives and are resent with the same request id and `message_id`, so the server stores them once.
- The driver holds only a weak handle on its own command queue (for `Message::ack`), so it stops when every `Client` and `Subscription` is dropped.
- An `unsubscribed` with no id means the server ended the subscription (an admin revoked access): the topic is dropped, ending its `Subscription`, and `Event::Unsubscribed` is sent. So every unsubscribe the driver sends carries an id; never send one without.
- Close codes decide reconnection: `CloseCode::should_reconnect`, except that `TokenExpired` reconnects only with password credentials.
- A full subscription buffer blocks the driver, which stops reading the socket; the server's backpressure then paces this connection. That is deliberate.

## Gaps

- `ws://` only. `wss://` needs a TLS feature on `tokio-tungstenite`; until then, run the client next to the server or behind a TLS-terminating proxy.
- No metrics hooks yet; use `Client::events`.

## Tests

- `make test-crate CRATE=popsub_sdk`. The tests start a server on a free port; the restart test reopens the same `FileStorage` directory once the first server lets go of it.
