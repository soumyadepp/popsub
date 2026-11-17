# The scenario driver protocol

A driver is a small program, shipped with each client library, that lets `popsub-scenarios` operate that library. The harness starts one driver process per client it needs, writes commands to its standard input and reads responses and events from its standard output.

## Framing

- One JSON object per line, in both directions, UTF-8, `\n` terminated. Flush after every line.
- Anything else the driver wants to print (logs, debugging) goes to standard error.
- The driver exits when standard input closes, or after answering `exit`.
- Commands can overlap: the harness may send a command while an earlier one is still waiting (a `publish_confirmed` held while the server is down, say). Handle each command concurrently and answer when it completes. Responses can come out of order; the `id` matches them.
- One driver holds at most one client. Multi-client scenarios start several drivers.

## Commands and responses

A command is `{"id": <integer>, "op": "<name>", ...arguments}`. Every command gets exactly one response:

```json
{"id": 7, "ok": true, ...results}
{"id": 7, "ok": false, "error": {"kind": "server", "code": "invalid_topic", "message": "..."}}
```

`error.kind` is one of:

| Kind | When | Extra fields |
|---|---|---|
| `server` | The server refused the request with an `error` frame | `code`: the error code, such as `not_authorized` |
| `closed` | The client has stopped for good | `close_code`: the close code that stopped it, or `null` |
| `timeout` | The request didn't complete within the request timeout | |
| `connect` | The server couldn't be reached, or the connection dropped before an admin request's reply | |
| `protocol` | The server sent something the client didn't expect | |
| `config` | The configuration was invalid | |

`message` is free text for people, on every kind.

### `connect`

```json
{"id": 1, "op": "connect", "url": "ws://127.0.0.1:40123", "username": "admin", "password": "password",
 "session_id": "phone", "clean_start": false, "ack_mode": "auto",
 "reconnect": {"initial_ms": 20, "max_ms": 200, "max_attempts": null}, "request_timeout_ms": 10000}
```

Creates the client and waits for its first connection. `session_id` may be absent or `null` (ephemeral). `ack_mode` is `auto` or `manual`. `max_attempts` is an integer or `null` (keep trying). Responds `{"ok": true}` once connected and authenticated. A rejected login is a `server` error with code `invalid_credentials`.

From here on the driver forwards the client's events (below) until it exits.

### `subscribe`

```json
{"id": 2, "op": "subscribe", "topic": "orders", "from": "new"}
```

`topic` may be a pattern (`orders/#`); the `message` events then carry each message's own topic, and `subscription_ended` and `unsubscribe` use the pattern. `from` is the protocol's `StartFrom`: `"resume"`, `"new"`, `{"last": 10}`, `{"since_ms": 1794800000000}` or `{"seq": 42}`. Responds `{"ok": true}` once `subscribed` arrives. From then on every message the application receives on the topic is reported as a `message` event, and the end of the subscription as `subscription_ended`.

### `unsubscribe`

`{"id": 3, "op": "unsubscribe", "topic": "orders"}`. Responds `{"ok": true}`.

### `publish`

`{"id": 4, "op": "publish", "topic": "orders", "payload": "text"}`. A QoS 0 publish. Responds `{"ok": true}` once the client has accepted it.

### `publish_confirmed`

`{"id": 5, "op": "publish_confirmed", "topic": "orders", "payload": "text"}`. A QoS 1 publish. Responds `{"ok": true, "seq": 12, "message_id": "..."}` once the server confirms it.

### `ack`

`{"id": 6, "op": "ack", "topic": "orders", "seq": 12}`. Acknowledges a message the driver reported earlier, through the library's own acknowledgement call on that message (manual mode). Responds `{"ok": true}`. Acknowledging a message the driver never reported is a `protocol` error.

### `admin`

```json
{"id": 8, "op": "admin", "method": "grant", "params": {"username": "rita", "pattern": "news/#", "access": "subscribe"}}
```

Calls the library's admin method. `method` and the names in `params` are the protocol's own (`create_user` takes `username`, `password` and `role`; `role` is `{"kind": "user", "topics": [...]}` as on the wire). Responds `{"ok": true, "result": ...}` with the user object for most methods, a list of them for `list_users`, and `null` for `delete_user`. The topic methods are `list_topics` (no params; a list of topic objects), and `get_topic` and `purge_topic` (`topic`; one topic object, `{"name", "first_seq", "last_seq", "messages", "subscribers"}`).

### `close` and `exit`

`{"id": 9, "op": "close"}` closes the client and responds `{"ok": true}`. `{"id": 10, "op": "exit"}` responds `{"ok": true}` and exits.

## Events

Events have no `id`: `{"event": "<name>", ...}`.

| Event | Fields | When |
|---|---|---|
| `message` | `topic`, `seq`, `payload`, `message_id`, `qos` | The library handed a message to the application |
| `subscription_ended` | `topic` | The application's subscription to the topic ended without the application asking (revoked access, or the client stopped) |
| `connected` | `resumed`: topic list | A connection came up |
| `disconnected` | `code`: close code or `null`, `reconnecting`: boolean | A connection went down |
| `unsubscribed` | `topic` | The server ended a subscription |
| `truncated` | `topic`, `first_available` | History was trimmed before the client got it |
| `stalled` | `topic`, `seq` | A message ran out of retries |
| `server_error` | `code`, `message` | An error that answers no request |

Event names and fields are fixed; report every one the library produces. A library may produce others (`auth_expiring`, `auth_refreshed`); report them with the library's names, and the harness ignores what it doesn't use.
