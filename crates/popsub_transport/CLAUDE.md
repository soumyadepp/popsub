# popsub_transport

The WebSocket edge. Accepts connections, speaks the JSON protocol, handles login and JWT auth, and turns requests into broker calls.

## Files

- `src/message.rs`: `ClientMessage` and `ServerMessage` (serde, tagged by `type`) and JWT `Claims`. This is the protocol.
- `src/websocket.rs`: `start_websocket_server`. Per connection: a reader loop, a send loop draining the client's channel, and cleanup.
- `src/tests.rs`: message serialisation tests.
- `src/websocket_tests.rs`: starts a real server on a free port (`portpicker`) and drives it with `tokio-tungstenite`.

## How a frame is handled

1. Parse JSON outside the broker lock.
2. Take the lock, find this connection's `Client`, run `handle_parsed_message` for login, auth and the auth gate. It returns whether to keep the connection and an optional `BrokerAction`.
3. Release the lock, then take it again to apply the `BrokerAction` (subscribe, unsubscribe, publish, ack).

## Invariants

- Nothing but `login` and `auth` is accepted before `client.authenticated` is true. Failing `auth`, or sending anything else first, closes the connection. A failed `login` does not.
- Never hold the broker lock across an `.await`. Sends to the client go through its unbounded `mpsc` channel, which is synchronous.
- Cleanup runs once per connection, guarded by an `AtomicBool`, whichever side notices the disconnect first.

## Gotchas

- Delivered messages are the broker's `Message` serialised directly, with no `type` field. `ServerMessage::Message` is defined but unused. If you fix this, it is a protocol change: update the README, `docs/DESIGN.md` section 4 and the tests together.
- Credentials are a single username and password from config. JWT expiry is checked only at `auth`.
- The unmerged `feat/add-auth` branch rewrites most of `websocket.rs`. Rebase carefully if you touch this file.

## Tests

- `make test-crate CRATE=popsub_transport`. The WebSocket tests bind real ports; flaky failures are usually a port or timing issue, not logic.
