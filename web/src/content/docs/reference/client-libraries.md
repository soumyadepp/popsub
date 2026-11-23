---
title: Client libraries
description: The PopSub client libraries for Rust, TypeScript, Python and Go, how to install each, and what they handle for you.
sidebar:
  order: 3
---

There are four client libraries, all in the
[PopSub repository](https://github.com/soumyadepp/popsub) and all tested against the same
scenarios, so they behave the same way.

| Language | Install | Needs | Docs |
|---|---|---|---|
| TypeScript | `npm install popsub` | Node 22+, or a browser | [README](https://github.com/soumyadepp/popsub/blob/main/clients/typescript/README.md) |
| Python | `pip install popsub` | Python 3.10+ | [README](https://github.com/soumyadepp/popsub/blob/main/clients/python/README.md) |
| Go | `go get github.com/soumyadepp/popsub/clients/go@latest` | Go 1.23+ | [pkg.go.dev](https://pkg.go.dev/github.com/soumyadepp/popsub/clients/go) |
| Rust | `cargo add popsub_sdk` | stable Rust | [docs.rs](https://docs.rs/popsub_sdk) |

The TypeScript client has no dependencies and uses the built-in `WebSocket`. Python depends
only on `websockets`, and Go only on `github.com/coder/websocket`.

## What they handle for you

Your code connects, subscribes, publishes and handles messages. The library deals with the
network going away.

When a connection drops, it reconnects with a randomised backoff and subscribes to each topic
again from the next `seq` it was expecting, so nothing that's still in the log is missed.
Confirmed publishes that hadn't been answered are resent with the same `message_id`, so the
server stores them once. It never retries after a close that means "stop", such as a disabled
account or a session taken over by another process.

It hands you each message once, even when the server sends it twice, by remembering the last
`seq` per topic. With QoS 1 it acknowledges as it hands messages over, or leaves that to you in
manual mode. If your code is slow, it stops reading from the socket rather than buffering
without limit, and the server paces itself to match.

Given a username and password, it logs in again before the token expires, without dropping the
connection. Given only a token, it can't, and the connection ends when the token does.

The full list is in
[`clients/README.md`](https://github.com/soumyadepp/popsub/blob/main/clients/README.md), which
every library is held to.

## Examples

Each library has a subscriber and a publisher in its `examples` folder, and any of them can talk
to any other. [`examples/README.md`](https://github.com/soumyadepp/popsub/blob/main/examples/README.md)
shows how to run them against a local server. They read `POPSUB_URL`, `POPSUB_USERNAME` and
`POPSUB_USER_PASSWORD`, defaulting to `ws://127.0.0.1:8080`, `alice` and `alice-password`.

For another language, see [Protocol](/reference/protocol/).
