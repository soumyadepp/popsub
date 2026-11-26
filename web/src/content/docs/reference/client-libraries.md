---
title: Client libraries
description: The PopSub client libraries for TypeScript, Python, Go and Rust, how to install each, and what they handle for you.
---

<p class="lede">
  Four client libraries, all in the PopSub repository, all tested against the same scenarios,
  so they behave the same way.
</p>

| Language | Install | Needs | API docs |
|---|---|---|---|
| TypeScript | `npm install popsub` | Node 22+, or a browser | [README](https://github.com/soumyadepp/popsub/blob/main/clients/typescript/README.md) |
| Python | `pip install popsub` | Python 3.10+ | [README](https://github.com/soumyadepp/popsub/blob/main/clients/python/README.md) |
| Go | `go get github.com/soumyadepp/popsub/clients/go@latest` | Go 1.23+ | [pkg.go.dev](https://pkg.go.dev/github.com/soumyadepp/popsub/clients/go) |
| Rust | `cargo add popsub_sdk` | Stable Rust | [docs.rs](https://docs.rs/popsub_sdk) |

Dependencies are minimal:

- **TypeScript:** none. It uses the built-in `WebSocket`.
- **Python:** only `websockets`.
- **Go:** only `github.com/coder/websocket`.

## What they handle for you

Your code connects, subscribes, publishes and handles messages. The library deals with the
network.

**When the connection drops:**

- It reconnects with a randomised backoff.
- It subscribes to each topic again from the next `seq` it expected, so nothing still kept is
  missed.
- It resends unanswered confirmed publishes with the same `message_id`, so each is stored once.
- It never reconnects after a close that means "stop", such as a disabled account or a session
  taken over by another process.

**While messages arrive:**

- It hands you each message once, even if the server sends it twice, by remembering the last
  `seq` per topic.
- With QoS 1 it acknowledges as it hands messages over, or leaves that to you in manual mode.
- If your code is slow, it stops reading from the socket instead of buffering without limit,
  and the server slows down to match.

**Logins:**

- Given a username and password, it logs in again before the token expires, without dropping
  the connection.
- Given only a token, it can't. The connection ends when the token does.

The full contract every library is held to is
[`clients/README.md`](https://github.com/soumyadepp/popsub/blob/main/clients/README.md).

## Examples

The [Examples](/examples/) section has complete programs in all four languages, one page per
use case. Each library also has a short subscriber and publisher in its own `examples` folder,
described in
[`examples/README.md`](https://github.com/soumyadepp/popsub/blob/main/examples/README.md).

For a language without a library, see [Protocol](/reference/protocol/).
