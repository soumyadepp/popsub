# PopSub client libraries

| Language | Directory | Status |
|---|---|---|
| Rust | [`crates/popsub_sdk`](../crates/popsub_sdk) | Passes the scenario suite |
| TypeScript (browsers, Node 22+) | [`clients/typescript`](typescript) | Passes the scenario suite |
| Python 3.10+ | [`clients/python`](python) | Passes the scenario suite |
| Go 1.23+ | [`clients/go`](go) | Passes the scenario suite |

Each client's README covers installing it, its API, and its own checks. CI (`.github/workflows/clients.yml`) runs every client's lint, type check and tests and the whole scenario suite against it, on the oldest and newest language versions it supports.

| Client | Driver command, from the repository root |
|---|---|
| Rust | `target/debug/popsub-sdk-driver` |
| TypeScript | `node clients/typescript/dist/driver.js` (build it with `npm run build` in `clients/typescript`) |
| Python | `uv run --project clients/python python -m popsub._driver` |
| Go | `clients/go/bin/popsub-driver` (build it with `make -C clients/go build`) |

Every client library behaves the same way, and proves it by passing one shared scenario suite (below). The wire protocol is in [`docs/DESIGN.md`](../docs/DESIGN.md) section 4, with JSON Schemas in [`docs/protocol/`](../docs/protocol).

## What every client does

A client library hides the protocol's bookkeeping, so an application only sees connect, subscribe, publish, receive and acknowledge.

**Connecting**

- Logs in with a username and password (or takes a token), then sends `auth` with `protocol: 1` and the optional `session_id` and `clean_start`. `clean_start` is only sent on the first connection, never on reconnects.
- Reads `limits` from `authenticated`. If nothing arrives from the server for `idle_timeout_secs + heartbeat_secs`, the connection is dead: drop it and reconnect. The server pings every `heartbeat_secs`; a client that can't see WebSocket pings (a browser's `WebSocket`) sends a `ping` request instead when it has heard nothing for `heartbeat_secs`, and counts the `pong`.
- With password credentials, answers `auth_expiring` by logging in again and sending `auth` with the new token on the same connection.

**Reconnecting**

- When the connection drops, reconnects with jittered exponential backoff: a random wait between zero and a ceiling that doubles from `initial` up to `max`, optionally giving up after `max_attempts`.
- The close code decides whether to try. Never reconnect after `session_taken_over` (4003), `unsupported_protocol` (4006), `auth_failed` (4001) or `account_disabled` (4009). After `token_expired` (4004), reconnect only with password credentials. Otherwise reconnect, including when there is no close code.
- A failed first connection with a rejected login (`invalid_credentials`) or other non-temporary refusal fails `connect` instead of retrying.
- After reconnecting: topics listed in `authenticated.resumed` are left to the server. Every other subscribed topic is subscribed again from `{"seq": next_expected}`, never from the original `from`, so nothing still in the log is missed. Confirmed publishes still waiting for `published` are resent with the same request id and `message_id`, so the server stores them once. QoS 0 publishes made while disconnected (up to a bound) are sent.

**Receiving**

- Each subscribed topic remembers the next `seq` it expects. A message below it is a duplicate: never hand it to the application, but acknowledge it again if it was acknowledged before (or always, in automatic mode), so a lost ack can't stall the subscription.
- `subscribed.replay_from` (and `truncated.first_available`) move the next expected `seq` forward.
- Acknowledgement modes: automatic (ack QoS 1 messages as they are handed over) and manual (the application calls `ack` on the message; acking twice is harmless).
- A full application buffer pauses reading the socket, so the server's backpressure paces the connection. Never buffer without bound.
- An `unsubscribed` frame with no `id` means the server ended the subscription (access revoked): end that subscription for the application and report it. Because of this, every `unsubscribe` the client sends carries an `id`.

**Wildcard subscriptions**

- `subscribe` accepts a pattern (`orders/#`, `sensors/+/temp`). The application gets one subscription for it, which yields the messages of every matching topic, each under its own topic name and `seq`.
- The server announces each topic it joins for the pattern with a `subscribed` frame that has no `id`. The client then tracks that topic like any other (its own next expected `seq`, its own acknowledgements), owned by the pattern. A `subscribed` or `message` for a topic the client isn't tracking is matched against its patterns (the oldest matching one owns it); if none matches, a message is acknowledged and dropped as before.
- After reconnecting: resubscribe every tracked topic that wasn't resumed from `{"seq": next_expected}` first, as usual, and then send each pattern again with `from: {"seq": 1}`. The server skips topics the connection holds, so this joins only topics created during the disconnect, from their first message.
- Unsubscribing a pattern (or dropping its subscription) sends `unsubscribe` for the pattern and for each topic it owned, and stops tracking them.
- When the server ends one owned topic (`unsubscribed` with no `id`), only that topic stops; the pattern's subscription carries on.

**Requests**

- Every request that expects a reply carries an `id`; replies and errors are matched by it. Errors surface with their `code` (and `retry_after` for limits).
- `publish_confirmed` generates a `message_id` (a UUID) and resolves with `seq` and `message_id` once `published` arrives.
- Admin requests (`create_user`, `get_user`, `list_users`, `set_role`, `grant`, `revoke`, `set_password`, `set_enabled`, `delete_user`, `list_topics`, `get_topic`, `purge_topic`) are never queued while disconnected and never resent: a disconnect fails them, because the server may or may not have carried them out.
- Requests time out after a configurable limit.

**Reporting**

Clients expose events for: connected (with resumed topics), disconnected (with the close code and whether it will reconnect), truncated, stalled, auth expiring, auth refreshed, unsubscribed by the server, and server errors that answer no request.

## The scenario suite

`popsub-scenarios` (in [`crates/popsub_scenarios`](../crates/popsub_scenarios)) starts a real PopSub server for each scenario, which it can pause, restart and publish to directly, and drives a client through a small program called a driver. Each client library ships a driver; the protocol it speaks is in [`DRIVER.md`](DRIVER.md).

```bash
cargo build -p popsub_scenarios
target/debug/popsub-scenarios --driver "target/debug/popsub-sdk-driver"
target/debug/popsub-scenarios --driver "node clients/typescript/dist/driver.js" --scenario reconnect_resubscribes_without_gaps
```

It prints one line per scenario and exits with a failure status if any fail. `--list` prints the scenarios.
