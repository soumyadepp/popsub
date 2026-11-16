# popsub_transport

The WebSocket edge. Accepts connections, speaks the protocol defined in `popsub_protocol`, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls.

## Files

- `src/websocket.rs`:
  - `Server`: what every connection shares (a `SharedBroker`, the auth service, `RateLimiters`). Build with `Server::new` or `Server::from_settings`, swap limits with `with_rate_limiters`, then `listen(addr)` or `serve(listener)`.
  - `Session`: one connection. `handle` dispatches a `ClientFrame` to one method per message type, passing the request `id`; `require_user` is the single "is this connection logged in" check; `reply(id, ...)` and `fail(id, code, ...)` queue frames on the session's own channel.
  - `RateLimiters` and its defaults, the prune task, `registration_role` and `registration_error_message`.
- `src/websocket_tests.rs`: starts a real server on port 0 with `MemoryStorage` and drives it with `tokio-tungstenite`. `settle` waits until the server has handled a connection's earlier frames.

## Invariants

- `register`, `login` and `auth` are accepted before authentication. Anything else first gets `must authenticate first` and closes the connection, as does a failed `auth` or a client that is no longer registered. Permission denials and rate limits send an error and keep it open.
- Subscribe, unsubscribe, publish, ack and session attachment are async `SharedBroker` calls. Auth state goes through `SharedBroker::with`. The auth service is a plain `Arc<AuthService>` with no lock of its own.
- If storage fails, a QoS 1 publisher gets `storage unavailable` and a failed subscribe gets an error and is not subscribed. A QoS 1 publish that succeeds gets `published` with its `seq`.
- `authenticate` with a `session_id` replies `authenticated` first, then restores each stored subscription the user may still read (forgetting the rest), so the client sees the reply before any restored history.
- `ack` takes a topic and `seq`; ACKs for records not in flight to the connection are ignored and only logged.
- Replies go through `Session::reply` or `Session::fail`, never through the broker's client map, and always carry the request's `id`. Every failure has an `ErrorCode`.
- A frame that doesn't parse gets `bad_request` (with the `id` if `salvage_request_id` can read one) and the connection stays open.
- Publish checks the topic permission before the rate limit, so denied publishes don't spend a user's allowance.
- Failed logins are limited per `address|username` and per address; registration per address; publishing per username. See `docs/DESIGN.md` section 10.
- Registration errors go through `registration_error_message`, which hides whether a username is taken.

## Gotchas

- `message`, `truncated`, `subscribed` and `stalled` frames are written by the broker, not by `Session::reply`. The matching `ServerMessage` variants wrap the broker's structs so clients parse everything through one enum.
- Session ids are checked by `valid_session_id` (1 to 128 characters, no control characters) before anything is attached.
- Limits use the socket address, so behind a proxy every client shares one.
- JWT expiry is checked only at `auth`.

## Tests

- `make test-crate CRATE=popsub_transport`. Tests bind port 0 and pass the bound listener to `Server::serve`, so there is no port race and no startup sleep. Use `wait_for_no_clients` rather than asserting on cleanup straight after a close.
