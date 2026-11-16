# popsub_connection

`Client` is the server's record of one connected socket, and `Outbox` is its outgoing queue. Client libraries for PopSub live elsewhere (`popsub_sdk`).

## What it holds

- `id`: a new UUID per connection. Reconnecting produces a different id; continuity across connections comes from `session`, not the id.
- `sender`: the connection's `Outbox` (`src/outbox.rs`), an outgoing queue with a byte budget and a close signal. The broker and transport push frames into it, and the transport's send loop drains the matching `OutboxReceiver` into the socket.
- `authenticated`: set by the transport after a valid JWT.
- `username`: set alongside `authenticated`; the transport uses it for permission checks and the publish rate limit.
- `session`: the storage key of the durable session the connection holds (`popsub_broker::session_key`), or `None` for an ephemeral connection. Subscriptions and cursors outlive the connection only when this is set.

## Gotchas

- Above the soft limit `has_room` is false: callers that can wait (deliveries, catch-up) must check it and leave the work to the log; `ready()` wakes when the queue drains. `send` ignores the soft limit but refuses anything over the hard limit and closes the connection as `SlowConsumer`.
- Every close goes through `Outbox::close`, first reason wins. Queued frames are written before the close frame, except for a slow consumer, whose backlog is dropped.
- `OutboxReceiver::try_recv` keeps returning the close once closing; test helpers must stop at the first `Outgoing::Close`.
