# popsub_transport

The WebSocket edge. Accepts connections, speaks the JSON protocol, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls.

## Files

- `src/message.rs`: `ClientMessage` and `ServerMessage` (serde, tagged by `type`). This is the protocol.
- `src/websocket.rs`:
  - `Server`: what every connection shares (a `SharedBroker`, the auth service, `RateLimiters`). Build with `Server::new` or `Server::from_settings`, swap limits with `with_rate_limiters`, then `listen(addr)` or `serve(listener)`.
  - `Session`: one connection. `handle` dispatches to one method per message type; `require_user` is the single "is this connection logged in" check; `reply` queues frames on the session's own channel.
  - `RateLimiters` and its defaults, the prune task, `registration_role` and `registration_error_message`.
- `src/tests.rs`: the JSON wire format of client and server frames.
- `src/websocket_tests.rs`: starts a real server on port 0 and drives it with `tokio-tungstenite`.

## Invariants

- `register`, `login` and `auth` are accepted before authentication. Anything else first gets `must authenticate first` and closes the connection, as does a failed `auth` or a client that is no longer registered. Permission denials and rate limits send an error and keep it open.
- The auth lock is always released before the broker is touched, and the broker is only reached through `SharedBroker::with`.
- Replies go through `Session::reply`, never through the broker's client map.
- Publish checks the topic permission before the rate limit, so denied publishes don't spend a user's allowance.
- Failed logins are limited per `address|username` and per address; registration per address; publishing per username. See `docs/DESIGN.md` section 10.
- Registration errors go through `registration_error_message`, which hides whether a username is taken.

## Gotchas

- Delivered messages are the broker's `Message` serialised directly, with no `type` field. `ServerMessage::Message` is defined but unused. Fixing this is a protocol change: update the README, `docs/DESIGN.md` section 4 and the tests together.
- `ack` is matched by message id only; any authenticated client can acknowledge any pending message.
- Limits use the socket address, so behind a proxy every client shares one.
- JWT expiry is checked only at `auth`.

## Tests

- `make test-crate CRATE=popsub_transport`. Tests bind port 0 and pass the bound listener to `Server::serve`, so there is no port race and no startup sleep. Use `wait_for_no_clients` rather than asserting on cleanup straight after a close.
