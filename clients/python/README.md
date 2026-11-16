# popsub (Python client)

An asyncio client for [PopSub](https://github.com/soumyadepp/popsub), a WebSocket publish/subscribe broker. It follows the behaviour every PopSub client shares (`clients/README.md` in the repository) and passes the shared scenario suite.

The client handles the protocol's bookkeeping so an application only connects, subscribes, publishes, receives and acknowledges:

- Logs in with a username and password (or takes a token) and authenticates with protocol version 1. With password credentials it logs in again when the server sends `auth_expiring`.
- Reconnects with jittered exponential backoff, and reads the close code to decide whether to: never after `auth_failed` (4001), `session_taken_over` (4003), `unsupported_protocol` (4006) or `account_disabled` (4009), and after `token_expired` (4004) only with password credentials. A rejected login on the first connection fails `connect` without retrying.
- Treats the connection as dead when nothing (pings included) arrives for `idle_timeout_secs + heartbeat_secs` from the server's `limits`.
- After reconnecting, leaves topics the server resumed for a durable session alone, subscribes every other topic again from `{"seq": next_expected}`, resends unconfirmed `publish_confirmed` calls with the same request id and `message_id`, and sends QoS 0 publishes held while disconnected (up to 10,000; the oldest are dropped first).
- Hands each `(topic, seq)` to the application once. A duplicate is acknowledged again when it was acknowledged before, or always in automatic mode.
- Stops reading the socket while a subscription's buffer is full, so the server paces delivery instead of the client buffering without bound.
- Ends a subscription and reports `Unsubscribed` when the server ends it (an `unsubscribed` frame with no id). Every unsubscribe the client sends carries an id for that reason.
- Never queues or resends admin requests: a disconnect fails them with `ConnectError`, because the server may or may not have carried them out.

Delivery is what the server provides: QoS 1 is at-least-once per subscriber while connected, and across reconnects only for durable sessions whose messages are still retained. The client removes the duplicates that this produces.

## Install

Python 3.10 or newer. The only runtime dependency is [`websockets`](https://websockets.readthedocs.io/).

The package isn't on PyPI yet. Install it from a checkout of the repository:

```bash
uv add path/to/popsub/clients/python
```

or `pip install path/to/popsub/clients/python`.

## Usage

```python
import asyncio

from popsub import AckMode, Client, ClientConfig, PasswordCredentials, StartFrom


async def main() -> None:
    config = ClientConfig(
        "ws://127.0.0.1:8080",
        PasswordCredentials("alice", "alice-password"),
        session_id="alice-laptop",
        ack_mode=AckMode.MANUAL,
    )
    async with await Client.connect(config) as client:
        orders = await client.subscribe("orders", StartFrom.resume())
        stored = await client.publish_confirmed("orders", "order 42 shipped")
        print("stored as seq", stored.seq)
        async for message in orders:
            print(message.topic, message.seq, message.payload)
            message.ack()


asyncio.run(main())
```

Events arrive on a separate stream:

```python
async def watch(client: Client) -> None:
    async for event in client.events():
        print(event.to_json())
```

## API

Everything below is importable from `popsub`.

| Name | What it is |
|---|---|
| `await Client.connect(config)` | Connects and waits for the first authenticated connection, retrying with the reconnect policy for up to `request_timeout`. |
| `await client.subscribe(topic, start_from=StartFrom.resume())` | Returns a `Subscription`. `StartFrom.resume()`, `.new()`, `.last(n)`, `.since_ms(t)` or `.seq(s)`. |
| `await client.unsubscribe(topic)` | Unsubscribes; the topic's `Subscription` ends. |
| `await client.publish(topic, payload)` | QoS 0. Returns once queued; server failures arrive as `ServerErrorEvent`. |
| `await client.publish_confirmed(topic, payload)` | QoS 1 with a new UUID `message_id`. Returns `Published(seq, message_id)` once stored. |
| `client.admin()` | `Admin` with `create_user`, `get_user`, `list_users`, `set_role`, `grant`, `revoke`, `set_password`, `set_enabled`, `delete_user`. They return `UserInfo` (a list for `list_users`, None for `delete_user`). |
| `client.events()` | An `EventStream`: an async iterator of events from now on, ending after the client stops. It keeps the latest 256 events for a slow reader. |
| `await client.close()` | Closes the connection and stops the client. Every task it started has finished when this returns. |
| `client.stopped`, `client.close_code` | Whether the client stopped for good, and the close code that stopped it. |
| `Subscription` | `async for message in subscription`, or `await subscription.next()` (None at the end). `subscription.close()` stops listening and unsubscribes without waiting. `replay_from` and `replay_to` come from the server's `subscribed`. |
| `Message` | `topic`, `seq`, `message_id`, `payload`, `qos`, `published_at_ms`, and `ack()`, which does something only for QoS 1 in manual mode and is safe to call twice. |

Events: `Connected(resumed)`, `Disconnected(code, reconnecting)`, `Truncated(topic, first_available)`, `Stalled(topic, seq)`, `AuthExpiring(expires_at_ms)`, `AuthRefreshed()`, `Unsubscribed(topic)` and `ServerErrorEvent(code, message)`. Each has `to_json()`.

### Configuration

`ClientConfig(url, credentials, ...)`:

| Field | Default | Meaning |
|---|---|---|
| `url` | | `ws://` or `wss://` |
| `credentials` | | `PasswordCredentials(username, password)` or `TokenCredentials(token)` |
| `session_id` | `None` | A durable session to hold; None is ephemeral. |
| `clean_start` | `False` | Discard the session's stored subscriptions; sent on the first connection only. |
| `ack_mode` | `AckMode.AUTO` | `AUTO` acknowledges QoS 1 messages as they are handed over; `MANUAL` waits for `message.ack()`. |
| `reconnect` | `ReconnectPolicy(0.25, 30.0, None)` | `initial` and `max` ceilings in seconds, and `max_attempts` (None keeps trying). |
| `request_timeout` | `30.0` | Seconds a connect, subscribe, confirmed publish or admin call may take. |
| `subscription_buffer` | `1024` | Messages buffered per subscription before the client stops reading the socket. |
| `ssl_context` | `None` | TLS settings for `wss://`, such as a context trusting a private CA. |

### Errors

Every failure is a `PopSubError` with a `kind`:

| Class | `kind` | Extra attributes |
|---|---|---|
| `ServerError` | `server` | `code` (such as `not_authorized`), `retry_after` |
| `ClosedError` | `closed` | `close_code` (or None) |
| `RequestTimeoutError` | `timeout` | |
| `ConnectError` | `connect` | |
| `ProtocolError` | `protocol` | |
| `ConfigError` | `config` | |

`error.is_temporary` says whether trying again later might work. Errors keep their cause in `__cause__`.

### Logging

The client logs through the standard `logging` module under the `popsub` logger and configures no handlers. Its own log lines never include passwords, tokens or payloads. The `websockets` library logs under its own `websockets` logger, and at `DEBUG` level that includes frame contents, so keep it above `DEBUG` in production.

## Scenario suite

The scenario driver is the internal module `popsub._driver`; it is not part of the public API. From the repository root, with the suite built (`cargo build -p popsub_scenarios`):

```bash
target/debug/popsub-scenarios --driver "uv run --project clients/python python -m popsub._driver"
target/debug/popsub-scenarios --driver "uv run --project clients/python python -m popsub._driver" --scenario reconnect_resubscribes_without_gaps
```

The driver logs to standard error; set `POPSUB_LOG=DEBUG` for more.

## Development

Uses [uv](https://docs.astral.sh/uv/). From `clients/python`:

```bash
uv sync
uv run ruff format --check . && uv run ruff check . && uv run mypy && uv run pytest
```

That runs the formatter check, the linter, `mypy --strict` over the package and tests, and the tests. The tests cover backoff, deduplication and the wire types, and run the client against a small in-process fake server to check that resent copies are dropped and acknowledged again, that a reconnect resubscribes from the next expected seq, and that `close()` leaves no task running.

`uv build` writes the wheel and source distribution to `dist/`.
