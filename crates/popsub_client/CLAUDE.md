# popsub_client

Despite the name, this is not a client library. `Client` is the server's record of one connected socket.

## What it holds

- `id`: a new UUID per connection. Reconnecting produces a different id, which is why subscriptions don't survive a disconnect.
- `sender`: an unbounded `mpsc` sender of `tungstenite` messages. The broker and transport push frames into it, and the transport's send loop writes them to the socket.
- `authenticated`: set by the transport after a valid JWT.
- `username`: set alongside `authenticated`; the transport uses it for permission checks and the publish rate limit.

## Gotchas

- The channel is unbounded. A slow reader makes it grow without limit. Moving to a bounded channel changes every `send` call site in broker and transport, and needs a policy for what happens when it's full (see `docs/DESIGN.md` section 13).
- If a real client SDK is ever written, give it a different crate name, and consider renaming this one to something like `popsub_session`.
