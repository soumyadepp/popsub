"""Internal: the scenario driver for the Python client. Not part of the public API.

Reads commands from standard input and writes responses and events to
standard output, one JSON object per line, as `clients/DRIVER.md` in the
PopSub repository describes. `popsub-scenarios` runs it with
`python -m popsub._driver`. A publish command may carry `bytes` (base64) in
place of `payload`, which goes through the client's calls for publishing
bytes, and every `message` event reports the message's bytes in base64 as
`Message.data` returns them. Logs go to standard error; set `POPSUB_LOG` to a
level name such as `DEBUG` to see more.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import contextlib
import json
import logging
import os
import sys
from collections.abc import Awaitable, Callable, Coroutine
from typing import Any, TypeAlias

from popsub.admin import Admin
from popsub.client import Client
from popsub.config import AckMode, ClientConfig, PasswordCredentials, ReconnectPolicy
from popsub.errors import ClosedError, PopSubError, ProtocolError, ServerError
from popsub.message import Message, Subscription
from popsub.protocol import Access, RoleSpec, StartFrom

log = logging.getLogger("popsub.driver")

STDIN_LINE_LIMIT = 16 * 1024 * 1024
"""The longest command line accepted, in bytes."""

EXIT_WAIT = 6.0
"""Seconds to wait for the client to close when the driver exits."""

Command: TypeAlias = dict[str, Any]
"""One decoded command line."""

_OpHandler: TypeAlias = Callable[[Client, Command], Awaitable[dict[str, Any]]]
"""Carries out one op on a connected client and returns the result fields."""

_AdminCall: TypeAlias = Callable[[Admin, dict[str, Any]], Awaitable[Any]]
"""Calls one admin method with the command's params and returns the JSON result."""


class _DriverError(Exception):
    """A command failed in the driver itself; carries the response's `error` object.

    Attributes:
        error: The `error` object.
    """

    def __init__(self, kind: str, message: str) -> None:
        """Builds the failure.

        Args:
            kind: The error kind, such as `protocol` or `config`.
            message: What went wrong.
        """
        super().__init__(message)
        self.error: dict[str, Any] = {"kind": kind, "message": message}


def describe_error(error: PopSubError) -> dict[str, Any]:
    """Describes a client error as a driver `error` object.

    Args:
        error: The error.

    Returns:
        `{"kind", "message"}`, plus `code` for server errors and `close_code`
        for closed ones.
    """
    described: dict[str, Any] = {"kind": error.kind, "message": str(error)}
    if isinstance(error, ServerError):
        described["code"] = error.code
    if isinstance(error, ClosedError):
        described["close_code"] = error.close_code
    return described


def _seconds(value: Any, default_ms: int) -> float:
    """Converts milliseconds from a command to seconds.

    Args:
        value: The milliseconds, or None.
        default_ms: The milliseconds to use when `value` isn't a number.

    Returns:
        The seconds.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return default_ms / 1000.0
    return value / 1000.0


def config_from_command(command: Command) -> ClientConfig:
    """Builds a client configuration from a `connect` command.

    Args:
        command: The `connect` command.

    Returns:
        The configuration.
    """
    reconnect = command.get("reconnect") or {}
    max_attempts = reconnect.get("max_attempts")
    session_id = command.get("session_id")
    return ClientConfig(
        url=str(command.get("url", "")),
        credentials=PasswordCredentials(str(command.get("username", "")), str(command.get("password", ""))),
        session_id=session_id if isinstance(session_id, str) else None,
        clean_start=bool(command.get("clean_start", False)),
        ack_mode=AckMode.MANUAL if command.get("ack_mode") == "manual" else AckMode.AUTO,
        reconnect=ReconnectPolicy(
            initial=_seconds(reconnect.get("initial_ms"), 250),
            max=_seconds(reconnect.get("max_ms"), 30_000),
            max_attempts=max_attempts if isinstance(max_attempts, int) else None,
        ),
        request_timeout=_seconds(command.get("request_timeout_ms"), 30_000),
    )


def _bytes(command: Command) -> bytes | None:
    """Reads a publish command's `bytes`: the body to publish as bytes, in base64.

    Args:
        command: The command.

    Returns:
        The bytes, or None when the command carries a text `payload` instead.

    Raises:
        _DriverError: A `config` error when `bytes` isn't base64.
    """
    encoded = command.get("bytes")
    if encoded is None:
        return None
    if not isinstance(encoded, str):
        raise _DriverError("config", "bytes must be a base64 string")
    try:
        return base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError) as error:
        raise _DriverError("config", f"bytes isn't valid base64: {error}") from error


def _message_bytes(message: Message) -> str | None:
    """Reads a message's bytes through `Message.data`, for a `message` event.

    Args:
        message: The message.

    Returns:
        The bytes in base64, or None when the payload claims base64 and
        doesn't decode.
    """
    try:
        return base64.b64encode(message.data).decode("ascii")
    except ProtocolError:
        return None


async def _create_user(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `create_user`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The new user object.
    """
    role = RoleSpec.from_json(params.get("role"))
    user = await admin.create_user(str(params.get("username", "")), str(params.get("password", "")), role)
    return user.to_json()


async def _get_user(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `get_user`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The user object.
    """
    return (await admin.get_user(str(params.get("username", "")))).to_json()


async def _list_users(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `list_users`.

    Args:
        admin: The admin handle.
        params: Unused.

    Returns:
        The user objects.
    """
    return [user.to_json() for user in await admin.list_users()]


async def _set_role(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `set_role`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The user object.
    """
    role = RoleSpec.from_json(params.get("role"))
    return (await admin.set_role(str(params.get("username", "")), role)).to_json()


async def _grant(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `grant`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The user object.
    """
    access = Access(params.get("access"))
    return (await admin.grant(str(params.get("username", "")), str(params.get("pattern", "")), access)).to_json()


async def _revoke(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `revoke`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The user object.
    """
    return (await admin.revoke(str(params.get("username", "")), str(params.get("pattern", "")))).to_json()


async def _set_password(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `set_password`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The user object.
    """
    return (await admin.set_password(str(params.get("username", "")), str(params.get("password", "")))).to_json()


async def _set_enabled(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `set_enabled`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The user object.
    """
    return (await admin.set_enabled(str(params.get("username", "")), bool(params.get("enabled", True)))).to_json()


async def _delete_user(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `delete_user`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        None.
    """
    await admin.delete_user(str(params.get("username", "")))
    return None


async def _list_topics(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `list_topics`.

    Args:
        admin: The admin handle.
        params: Unused.

    Returns:
        The topic objects.
    """
    return [topic.to_json() for topic in await admin.list_topics()]


async def _get_topic(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `get_topic`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The topic object.
    """
    return (await admin.get_topic(str(params.get("topic", "")))).to_json()


async def _purge_topic(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `purge_topic`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The topic object after the purge.
    """
    return (await admin.purge_topic(str(params.get("topic", "")))).to_json()


async def _backup(admin: Admin, params: dict[str, Any]) -> Any:
    """Calls `backup`.

    Args:
        admin: The admin handle.
        params: The command's params.

    Returns:
        The backup object.
    """
    name = params.get("name")
    return (await admin.backup(name if isinstance(name, str) else None)).to_json()


ADMIN_CALLS: dict[str, _AdminCall] = {
    "create_user": _create_user,
    "get_user": _get_user,
    "list_users": _list_users,
    "set_role": _set_role,
    "grant": _grant,
    "revoke": _revoke,
    "set_password": _set_password,
    "set_enabled": _set_enabled,
    "delete_user": _delete_user,
    "list_topics": _list_topics,
    "get_topic": _get_topic,
    "purge_topic": _purge_topic,
    "backup": _backup,
}
"""The admin methods the driver can call, by protocol name."""


class Driver:
    """The driver's state: one client, held messages and topics being left.

    Attributes:
        client: The client, once connected.
        held: QoS 1 messages reported and not yet acknowledged, by topic and seq.
        leaving: Topics the harness unsubscribed from, whose end isn't reported.
        tasks: Background tasks, kept so they aren't garbage collected.
        output_closed: True once standard output stopped accepting lines.
    """

    def __init__(self) -> None:
        """Builds a driver with no client."""
        self.client: Client | None = None
        self.held: dict[tuple[str, int], Message] = {}
        self.leaving: set[str] = set()
        self.tasks: set[asyncio.Task[None]] = set()
        self.output_closed = False
        self._ops: dict[str, _OpHandler] = {
            "subscribe": self._subscribe,
            "unsubscribe": self._unsubscribe,
            "publish": self._publish,
            "publish_confirmed": self._publish_confirmed,
            "ack": self._ack,
            "admin": self._admin,
            "close": self._close,
        }

    def write(self, line: dict[str, Any]) -> None:
        """Writes one line to standard output and flushes it.

        Once the harness has stopped reading, standard output is pointed at
        the null device so later writes and the interpreter's final flush
        don't fail.

        Args:
            line: The response or event.
        """
        if self.output_closed:
            return
        try:
            sys.stdout.write(json.dumps(line) + "\n")
            sys.stdout.flush()
        except (BrokenPipeError, ValueError):
            self.output_closed = True
            devnull = os.open(os.devnull, os.O_WRONLY)
            os.dup2(devnull, sys.stdout.fileno())
            os.close(devnull)

    def spawn(self, coroutine: Coroutine[Any, Any, None]) -> None:
        """Runs a coroutine in the background, keeping a reference to it.

        Args:
            coroutine: The coroutine.
        """
        task = asyncio.ensure_future(coroutine)
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def answer(self, command: Command) -> None:
        """Carries out one command and writes its response.

        Args:
            command: The command.
        """
        request_id = command.get("id")
        try:
            result = await self.handle(command)
        except _DriverError as failure:
            self.write({"id": request_id, "ok": False, "error": failure.error})
        except PopSubError as error:
            self.write({"id": request_id, "ok": False, "error": describe_error(error)})
        else:
            self.write({**result, "id": request_id, "ok": True})

    async def handle(self, command: Command) -> dict[str, Any]:
        """Carries out one command.

        Args:
            command: The command.

        Returns:
            The response's result fields.

        Raises:
            _DriverError: For an unknown op, a command before `connect`, or
                invalid arguments.
            PopSubError: When the client call fails.
        """
        op = str(command.get("op"))
        if op == "connect":
            return await self._connect(command)
        if self.client is None:
            raise _DriverError("protocol", "connect first")
        handler = self._ops.get(op)
        if handler is None:
            raise _DriverError("protocol", f"unknown op {op!r}")
        try:
            return await handler(self.client, command)
        except (TypeError, ValueError) as error:
            raise _DriverError("config", f"invalid arguments for {op}: {error}") from error

    async def _connect(self, command: Command) -> dict[str, Any]:
        """Connects the client and starts forwarding its events.

        Args:
            command: The `connect` command.

        Returns:
            An empty result.

        Raises:
            _DriverError: When a client is already connected.
            PopSubError: When connecting fails.
        """
        if self.client is not None:
            raise _DriverError("protocol", "already connected; one driver holds one client")
        client = await Client.connect(config_from_command(command))
        self.spawn(self._forward_events(client))
        self.client = client
        return {}

    async def _subscribe(self, client: Client, command: Command) -> dict[str, Any]:
        """Subscribes and starts reporting the topic's messages.

        Args:
            client: The client.
            command: The `subscribe` command.

        Returns:
            An empty result.

        Raises:
            ValueError: When `from` isn't a valid start position.
        """
        topic = str(command.get("topic", ""))
        start = StartFrom.from_json(command.get("from"))
        self.leaving.discard(topic)
        group = command.get("group")
        if isinstance(group, str):
            subscription = await client.subscribe_shared(topic, group, start)
        else:
            subscription = await client.subscribe(topic, start)
        self.spawn(self._forward(subscription))
        return {}

    async def _unsubscribe(self, client: Client, command: Command) -> dict[str, Any]:
        """Unsubscribes, without reporting the subscription's end.

        Args:
            client: The client.
            command: The `unsubscribe` command.

        Returns:
            An empty result.
        """
        topic = str(command.get("topic", ""))
        self.leaving.add(topic)
        await client.unsubscribe(topic)
        return {}

    async def _publish(self, client: Client, command: Command) -> dict[str, Any]:
        """Publishes at QoS 0.

        Args:
            client: The client.
            command: The `publish` command.

        Returns:
            An empty result.
        """
        topic = str(command.get("topic", ""))
        data = _bytes(command)
        if data is None:
            await client.publish(topic, str(command.get("payload", "")))
        else:
            await client.publish_bytes(topic, data)
        return {}

    async def _publish_confirmed(self, client: Client, command: Command) -> dict[str, Any]:
        """Publishes at QoS 1.

        Args:
            client: The client.
            command: The `publish_confirmed` command.

        Returns:
            The `seq` and `message_id`.
        """
        topic = str(command.get("topic", ""))
        data = _bytes(command)
        if data is None:
            published = await client.publish_confirmed(topic, str(command.get("payload", "")))
        else:
            published = await client.publish_bytes_confirmed(topic, data)
        return {"seq": published.seq, "message_id": published.message_id}

    async def _ack(self, client: Client, command: Command) -> dict[str, Any]:
        """Acknowledges a reported message through `Message.ack`.

        Args:
            client: Unused; the message carries its own link to the client.
            command: The `ack` command.

        Returns:
            An empty result.

        Raises:
            _DriverError: When no such message was reported.
        """
        key = (str(command.get("topic", "")), int(command.get("seq", 0)))
        message = self.held.pop(key, None)
        if message is None:
            raise _DriverError("protocol", "no such message was reported")
        message.ack()
        return {}

    async def _admin(self, client: Client, command: Command) -> dict[str, Any]:
        """Calls an admin method.

        Args:
            client: The client.
            command: The `admin` command.

        Returns:
            `{"result": ...}`: a user object, a list of them, or None.

        Raises:
            _DriverError: For an unknown method.
        """
        method = str(command.get("method"))
        call = ADMIN_CALLS.get(method)
        if call is None:
            raise _DriverError("protocol", f"unknown admin method {method!r}")
        params = command.get("params") or {}
        return {"result": await call(client.admin(), params)}

    async def _close(self, client: Client, command: Command) -> dict[str, Any]:
        """Closes the client.

        Args:
            client: The client.
            command: Unused.

        Returns:
            An empty result.
        """
        await client.close()
        return {}

    async def _forward(self, subscription: Subscription) -> None:
        """Reports a subscription's messages, then its end unless the harness unsubscribed.

        Args:
            subscription: The subscription.
        """
        async for message in subscription:
            self.write(
                {
                    "event": "message",
                    "topic": message.topic,
                    "seq": message.seq,
                    "payload": message.payload,
                    "bytes": _message_bytes(message),
                    "message_id": message.message_id,
                    "qos": message.qos,
                }
            )
            if message.qos == 1:
                self.held[(message.topic, message.seq)] = message
        if subscription.topic not in self.leaving:
            self.write({"event": "subscription_ended", "topic": subscription.topic})

    async def _forward_events(self, client: Client) -> None:
        """Reports the client's events until it stops.

        Args:
            client: The client.
        """
        async for event in client.events():
            self.write(event.to_json())

    async def shutdown(self) -> None:
        """Closes the client and cancels the driver's own tasks."""
        if self.client is not None:
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.client.close(), EXIT_WAIT)
        tasks = list(self.tasks)
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.wait(tasks)


async def _stdin() -> asyncio.StreamReader:
    """Opens standard input as an asyncio stream.

    Returns:
        The reader.
    """
    loop = asyncio.get_running_loop()
    reader = asyncio.StreamReader(limit=STDIN_LINE_LIMIT)
    await loop.connect_read_pipe(lambda: asyncio.StreamReaderProtocol(reader), sys.stdin)
    return reader


async def main() -> None:
    """Reads commands until standard input closes or `exit`, handling each concurrently."""
    driver = Driver()
    reader = await _stdin()
    while True:
        line = await reader.readline()
        if not line:
            break
        try:
            command = json.loads(line)
        except ValueError:
            log.warning("ignoring a line that isn't JSON")
            continue
        if not isinstance(command, dict):
            log.warning("ignoring a line that isn't a JSON object")
            continue
        if command.get("op") == "exit":
            driver.write({"id": command.get("id"), "ok": True})
            break
        driver.spawn(driver.answer(command))
    await driver.shutdown()


def run() -> None:
    """Configures logging to standard error and runs the driver."""
    logging.basicConfig(
        stream=sys.stderr,
        level=os.environ.get("POPSUB_LOG", "WARNING").upper(),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    asyncio.run(main())


if __name__ == "__main__":
    run()
