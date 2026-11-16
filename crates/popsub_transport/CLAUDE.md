# popsub_transport

The WebSocket edge. Accepts connections, speaks the JSON protocol, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls.

## Files

- `src/message.rs`: `ClientMessage` and `ServerMessage` (serde, tagged by `type`). This is the protocol.
- `src/websocket.rs`: the server. `start_websocket_server(settings)` is what the binary calls; it builds the `AuthService` from settings and delegates to `start_websocket_server_with_auth` and `start_websocket_server_with_rate_limiting`. One `handle_*` function per message type, `RateLimiters` and its defaults, the prune task, and `registration_role`.
- `src/tests.rs`: message serialisation tests.
- `src/websocket_tests.rs`: starts a real server on a free port (`portpicker`) and drives it with `tokio-tungstenite`.

## Invariants

- `register`, `login` and `auth` are accepted before authentication. Anything else before a successful `auth` gets an error and closes the connection. A failed `auth` closes it. Permission denials and publish rate limits send an error and keep it open.
- Handlers never hold the broker lock and the auth lock together: read client state under the broker lock, release it, check permissions under the auth lock, release that, then take the broker lock again to act.
- Never hold the broker lock across an `.await`.
- Failed logins are limited per `address|username` and per address; registration per address; publishing per username. See `docs/DESIGN.md` section 10.
- Registration errors go through `registration_error_message`, which hides whether a username is taken.

## Gotchas

- Delivered messages are the broker's `Message` serialised directly, with no `type` field. `ServerMessage::Message` is defined but unused. Fixing this is a protocol change: update the README, `docs/DESIGN.md` section 4 and the tests together.
- Limits use the socket address, so behind a proxy every client shares one.
- JWT expiry is checked only at `auth`.

## Tests

- `make test-crate CRATE=popsub_transport`. The WebSocket tests bind real ports; flaky failures are usually a port or timing issue, not logic.
