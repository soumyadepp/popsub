# popsub_transport

The WebSocket edge. Accepts connections, speaks the protocol defined in `popsub_protocol`, runs register, login and auth, checks topic permissions and rate limits, and turns requests into broker calls.

## Files

- `src/websocket.rs`:
  - `Server`: what every connection shares (a `SharedBroker`, the auth service, `RateLimiters`). Build with `Server::new`, `Server::from_settings` (users in memory) or `Server::from_settings_with_users` (any `UserStore`, such as `FileUserStore`), swap limits with `with_rate_limiters`, trust proxies with `with_trusted_proxies`, then `listen(addr)` or `serve(listener)`.
  - `Session`: one connection. `handle` dispatches a `ClientFrame` to one method per message type, passing the request `id`; `require_user` is the single "is this connection logged in" check; `reply(id, ...)` and `fail(id, code, ...)` queue frames on the session's own channel.
  - `RateLimiters` (`from_settings` builds it from `[limits]`; `default` is the same values), `Heartbeat` (`with_heartbeat`), the prune task, `registration_role` and `registration_error_message`.
  - `run_connection`: the per-connection loop. It selects over client frames (`Session::on_frame`), the heartbeat tick, the token expiry warning and the token expiry, and ends with a `Flow`: `Continue`, `Close(CloseCode)` (send a close frame, wait up to `CLOSE_GRACE` for the reply) or `Stop` (no close frame).
- `src/health.rs`: `Health`, the `/healthz` and `/readyz` endpoints on their own listener (hyper, HTTP/1.1 only). `drain` marks the server as shutting down; readiness also fails while `SharedBroker::storage_failing(STORAGE_FAILURE_WINDOW)` reports failing appends.
- `src/tls.rs`: `ServerTls`, the certificate and key from PEM (`from_pem_files`, `from_pem`), on rustls with `ring` and an explicit provider.
- `src/proxy.rs`: `TrustedProxies`, parsed from `server.trusted_proxies` (addresses and CIDR ranges, IPv4-mapped addresses matching IPv4 entries), `client_address`, which walks `X-Forwarded-For` from the right past trusted hops, and `read_proxy_header`, which reads one PROXY protocol header (version 1 or 2) and nothing after it.
- `src/websocket/admin.rs`: the admin requests. `Session::admin` checks the caller is an enabled admin in the store (not just the token's role claim), then `run_admin` carries the request out. `role_spec` and `role_from_spec` convert between `Role` and the protocol's `RoleSpec`. `keep_an_admin` refuses to disable, delete or demote the last enabled admin. `end_disallowed_subscriptions` unsubscribes the account's live subscriptions it can no longer read and sends each connection `unsubscribed` with no id; disabling or deleting calls `SharedBroker::close_user` with `AccountDisabled`.
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
- Topic names are checked by `valid_topic` on subscribe and publish (1 to `MAX_TOPIC_BYTES`, no control characters, `+` or `#`). Publishes check the topic, payload size and `message_id` length before permissions and the rate limit.
- Each connection gets an `Outbox` with `queue_limits` (from `server.outbound_queue_bytes`). The send loop writes `Outgoing::Frame`s and stops after an `Outgoing::Close`; the connection loop also ends when the `Outbox` starts closing from elsewhere, and then does the close handshake.
- `serve` hands each socket to `open_connection`, which reads a PROXY header first when `with_proxy_protocol` is on and the peer is trusted (its address becomes the connection's `source`, which `X-Forwarded-For` starts from), then does the TLS handshake when the server has `with_tls`, then `run_connection`, which is generic over the stream. The TLS and WebSocket handshakes share one deadline.
- `serve` takes a `ConnectionSlot` per accepted socket, held until its task ends; a connection accepted at the limit (`max_connections`, from the broker settings) is handshaken and closed with `TryAgainLater`. After the handshake an `AddressSlot` counts it against its client address (`server.max_connections_per_address`), refusing it the same way when the address is full. The handshake has `HANDSHAKE_TIMEOUT` (10 s, `with_handshake_timeout` in tests) to finish, and keeps the `X-Forwarded-For` headers for `TrustedProxies::client_address`.
- Connections are accepted with `websocket_config`, which caps a frame at twice the payload limit plus `FRAME_OVERHEAD_BYTES`; a larger frame closes the connection with `MessageTooBig`.
- `auth` looks the token's user up in the store and refuses a disabled or deleted account with `auth_failed`, closing with `AccountDisabled`.
- A repeat `auth` on an authenticated connection only refreshes the token (`refresh_token`), and only for the same user.
- Token expiry is enforced per connection from the token's `exp`, with no leeway; `auth_expiring` goes out `AUTH_EXPIRY_WARNING` before.

## Gotchas

- `message`, `truncated`, `subscribed` and `stalled` frames are written by the broker, not by `Session::reply`. The matching `ServerMessage` variants wrap the broker's structs so clients parse everything through one enum.
- Session ids are checked by `valid_session_id` (1 to 128 characters, no control characters) before anything is attached.
- Limits key on `Session::client_ip`: the socket peer, or the forwarded address when the peer is in `TrustedProxies`. Only headers from trusted peers are read.

## Tests

- `make test-crate CRATE=popsub_transport`. Lifecycle tests use `start_server_with` for a fast heartbeat, `short_token` for tokens that expire in seconds, and `next_close` to read close codes. Tests bind port 0 and pass the bound listener to `Server::serve`, so there is no port race and no startup sleep. Use `wait_for_no_clients` rather than asserting on cleanup straight after a close.
