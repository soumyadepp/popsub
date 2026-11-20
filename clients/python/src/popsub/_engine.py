"""The engine: one asyncio task per `Client` that owns the WebSocket.

It connects, logs in and authenticates, then reads frames and the client's
commands until the connection ends. The close code then decides what
happens: reconnect with jittered backoff, or stop for good
(`session_taken_over`, `unsupported_protocol`, `auth_failed`,
`account_disabled`, `token_expired` with a bare token, or a rejected login).
After reconnecting it subscribes every topic again from the next seq it
expects (topics a durable session restored are subscribed again from
`resume` when the server holds them, and otherwise left to the server unless
one is still being claimed), resends
confirmed publishes still waiting for `published` under the same request id
and `message_id`, and sends QoS 0 publishes held while disconnected.

Each delivered message is checked against the next seq its topic expects,
so a resend or replay never reaches the application twice. A duplicate that
was acknowledged before, or any duplicate in automatic mode, is acknowledged
again so a lost ack can't stall the subscription.

An `unsubscribed` frame with no id means the server ended the subscription
(access revoked), so every unsubscribe the engine sends carries an id.

A wildcard subscription (`orders/#`) is one pattern. The server announces
each topic it joins for it with a `subscribed` frame that has no id; the
engine then tracks that topic like any other, owned by the pattern, and puts
its messages on the pattern's subscription. A `subscribed` or `message` for
an untracked topic is given to the oldest matching pattern. Releasing a
pattern unsubscribes it and every topic it owned. Subscribing again to a
pattern the engine holds replaces the subscription: the topics it owned move
to the new one with the next seq they expect, and the earlier one ends.

A durable session can be held by a new process, before the application has
subscribed to anything. With a session id the `auth` frame carries
`hold_resumed`, asking the server to send nothing on the session's topics
until the engine subscribes to them. A server that does so answers
`authenticated` with `held`: nothing is unclaimed then, a subscribe to a
resumed topic is an ordinary subscribe, and after a reconnect the engine
subscribes again from `resume` to each tracked topic the server lists as
resumed.

A server that answers without `held` sends on the resumed topics straight
away. The topics it lists
as resumed that the engine isn't tracking are then unclaimed: their messages are
dropped without an acknowledgement, so the server keeps them. Subscribing to
one by name, or to a pattern that matches it, claims it. A claimed topic's
messages are still dropped unacknowledged until the `subscribed` that
answers the claim, because the subscribe replays them; a pattern claims each
matching topic with a subscribe by name from `resume`.

A shared subscription (a topic read through a group) gets only some of the
topic's messages, out of order. It finds duplicates in a bounded set of
delivered seqs, ignores `replay_from` and `truncated`, and rejoins its group
with `resume` after a reconnect.

While a subscription's buffer is full the engine waits before reading the
next frame; the server then paces delivery for this connection.
"""

from __future__ import annotations

import asyncio
import json
import logging
import random
import time
from collections import deque
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any, TypeAlias

from websockets.asyncio.client import ClientConnection, connect
from websockets.exceptions import ConnectionClosed, InvalidURI, WebSocketException

from popsub._state import Channel, TopicState, is_pattern, matches_pattern
from popsub.config import AckMode, ClientConfig, PasswordCredentials
from popsub.errors import (
    ClosedError,
    ConfigError,
    ConnectError,
    PopSubError,
    ProtocolError,
    RequestTimeoutError,
    ServerError,
)
from popsub.message import (
    AuthExpiring,
    AuthRefreshed,
    Connected,
    Disconnected,
    EventHub,
    Message,
    Published,
    ServerErrorEvent,
    Stalled,
    Truncated,
    Unsubscribed,
)
from popsub.protocol import PROTOCOL_VERSION, CloseCode, StartFrom

log = logging.getLogger("popsub")

OFFLINE_PUBLISHES = 10_000
"""How many QoS 0 publishes are held while disconnected; the oldest go first."""

DEFAULT_SILENCE = 120.0
"""Seconds of silence that end a connection before the server sends its limits."""

MAX_FRAME_BYTES = 64 * 1024 * 1024
"""The largest frame accepted from the server."""

CLOSE_WAIT = 2.0
"""Seconds to wait for the server's reply to the client's close frame."""

STOP_WAIT = 5.0
"""Seconds `close` waits for the engine to stop before cancelling its task."""


class _Connection(ClientConnection):
    """A WebSocket connection that records when the server last sent anything.

    Pings and other control frames count, which `recv` never shows.

    Attributes:
        last_heard: `time.monotonic()` when bytes last arrived.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Builds the connection.

        Args:
            *args: Passed to `ClientConnection`.
            **kwargs: Passed to `ClientConnection`.
        """
        super().__init__(*args, **kwargs)
        self.last_heard = time.monotonic()

    def data_received(self, data: bytes) -> None:
        """Stamps the arrival time, then hands the bytes on.

        Args:
            data: Bytes from the socket.
        """
        self.last_heard = time.monotonic()
        super().data_received(data)


@dataclass
class _Subscribe:
    """Subscribe and report the `subscribed` frame.

    Attributes:
        topic: The topic, or a wildcard pattern.
        start: Where replay starts.
        generation: Identifies this subscription.
        channel: Where the subscription's messages go.
        reply: Gets the `subscribed` frame or the error.
        group: The shared subscription to join, if any.
    """

    topic: str
    start: StartFrom
    generation: int
    channel: Channel[Message]
    reply: asyncio.Future[dict[str, Any]]
    group: str | None = None


@dataclass
class _Unsubscribe:
    """Unsubscribe and report `unsubscribed`.

    Attributes:
        topic: The topic.
        reply: Gets None or the error.
    """

    topic: str
    reply: asyncio.Future[None]


@dataclass
class _Forget:
    """Unsubscribe without waiting, when the application closed a subscription.

    Attributes:
        topic: The topic.
        generation: The subscription being closed.
    """

    topic: str
    generation: int


@dataclass
class _Publish:
    """Publish a message.

    Attributes:
        topic: The topic.
        payload: The body.
        message_id: The idempotency key of a confirmed publish; None for QoS 0.
        reply: Gets the confirmation of a confirmed publish; None for QoS 0.
    """

    topic: str
    payload: str
    message_id: str | None = None
    reply: asyncio.Future[Published] | None = None


@dataclass
class _Ack:
    """Acknowledge a message, from `Message.ack`.

    Attributes:
        topic: The topic.
        seq: The message's seq.
    """

    topic: str
    seq: int


@dataclass
class _Request:
    """Send an admin request and report its reply.

    Never queued while disconnected and never resent.

    Attributes:
        frame: The request, without an id.
        reply: Gets the reply frame or the error.
    """

    frame: dict[str, Any]
    reply: asyncio.Future[dict[str, Any]]


class _Close:
    """Close the connection and stop."""


_Command: TypeAlias = _Subscribe | _Unsubscribe | _Forget | _Publish | _Ack | _Request | _Close


@dataclass
class _PendingSubscribe:
    """A subscribe waiting for `subscribed`; its waiter is in `subscribe_waiters`.

    Attributes:
        topic: The topic.
    """

    topic: str


@dataclass
class _PendingUnsubscribe:
    """An unsubscribe waiting for `unsubscribed`.

    Attributes:
        topic: The topic.
        reply: Gets None or the error.
    """

    topic: str
    reply: asyncio.Future[None]


@dataclass
class _PendingPublish:
    """A confirmed publish waiting for `published`; its frame is in `unconfirmed`.

    Attributes:
        reply: Gets the confirmation or the error.
    """

    reply: asyncio.Future[Published]


@dataclass
class _PendingRequest:
    """An admin request waiting for its reply.

    Attributes:
        reply: Gets the reply frame or the error.
    """

    reply: asyncio.Future[dict[str, Any]]


class _PendingRelogin:
    """A login that refreshes the token on the current connection."""


class _PendingReauth:
    """The `auth` that installs the refreshed token."""


_Pending: TypeAlias = (
    _PendingSubscribe | _PendingUnsubscribe | _PendingPublish | _PendingRequest | _PendingRelogin | _PendingReauth
)

_FrameHandler: TypeAlias = Callable[["_Connection", dict[str, Any], "_Pending | None"], Awaitable[None]]
"""A handler for one type of server frame: connection, frame, the request it answers."""


@dataclass
class _Topic:
    """One tracked topic: subscribed by name, or joined through a pattern.

    Attributes:
        state: Its deduplication and acknowledgement state.
        channel: Where its messages go; the pattern's channel when owned.
        generation: Which subscription this is; the pattern's when owned.
        owner: The wildcard pattern the server joined it for, if any.
        group: The shared subscription it is read through, if any.
        claiming: True when the server held the topic for the session before
            the application subscribed, and the `subscribed` that answers
            the subscribe hasn't arrived. Until it does, the topic's
            messages come from the server's own resume and are dropped
            without an acknowledgement, because the subscribe replays them.
    """

    state: TopicState
    channel: Channel[Message]
    generation: int
    owner: str | None = None
    group: str | None = None
    claiming: bool = False


@dataclass
class _Pattern:
    """One wildcard subscription.

    Attributes:
        channel: Where the messages of every topic it owns go.
        generation: Which subscription this is; the oldest matching pattern
            owns a newly joined topic.
    """

    channel: Channel[Message]
    generation: int


@dataclass(frozen=True)
class _Ending:
    """How a served connection ended.

    Attributes:
        stop: True when the application closed the client.
        code: The server's close code when the connection was lost, if any.
    """

    stop: bool
    code: CloseCode | None = None


class _ConnectionLostError(Exception):
    """A write failed because the connection is gone.

    Attributes:
        code: The close code the server sent, if any.
    """

    def __init__(self, code: CloseCode | None) -> None:
        """Builds the signal.

        Args:
            code: The close code the server sent, if any.
        """
        super().__init__(code)
        self.code = code


class _ConnectFailedError(Exception):
    """Connecting failed.

    Attributes:
        error: The error to report, chained to `cause` when one is given.
        retry: Whether trying again could help.
    """

    def __init__(self, error: PopSubError, retry: bool, cause: BaseException | None = None) -> None:
        """Builds the failure.

        Args:
            error: The error to report.
            retry: Whether trying again could help.
            cause: The exception that caused it, recorded as `error.__cause__`.
        """
        super().__init__(error)
        if cause is not None:
            error.__cause__ = cause
        self.error = error
        self.retry = retry


def _close_code(error: ConnectionClosed) -> CloseCode | None:
    """Reads the server's close code from a closed-connection error.

    Args:
        error: The error `websockets` raised.

    Returns:
        The code, or None when no close frame arrived or the code isn't one
        PopSub defines.
    """
    received = error.rcvd
    return CloseCode.from_code(received.code if received is not None else None)


def _settle(future: asyncio.Future[Any], result: Any = None, error: BaseException | None = None) -> None:
    """Completes a future unless its waiter already gave up.

    Args:
        future: The future.
        result: Its result, when `error` is None.
        error: The exception to set instead.
    """
    if future.done():
        return
    if error is not None:
        future.set_exception(error)
    else:
        future.set_result(result)


def _server_error(frame: dict[str, Any]) -> ServerError:
    """Turns an `error` frame into an exception.

    Args:
        frame: The frame.

    Returns:
        The exception, with code, message and retry_after.
    """
    return ServerError(
        str(frame.get("code", "internal")),
        str(frame.get("message", "")),
        frame.get("retry_after"),
    )


def _refusal(frame: dict[str, Any]) -> _ConnectFailedError:
    """Turns an unexpected reply during connecting into a failure.

    Args:
        frame: What the server sent instead of the expected reply.

    Returns:
        The failure; retryable only for the server's temporary refusals.
    """
    if frame.get("type") == "error":
        error = _server_error(frame)
        return _ConnectFailedError(error, error.is_temporary)
    return _ConnectFailedError(ProtocolError(f"unexpected reply {frame.get('type')!r}"), True)


def _silence_limit(limits: Any) -> float:
    """How long to wait for anything from the server before giving up.

    Args:
        limits: The `limits` object from `authenticated`.

    Returns:
        `idle_timeout_secs` (at least 1) plus `heartbeat_secs`, in seconds,
        or `DEFAULT_SILENCE` when the limits are missing.
    """
    if not isinstance(limits, dict):
        return DEFAULT_SILENCE
    idle = limits.get("idle_timeout_secs")
    heartbeat = limits.get("heartbeat_secs")
    if not isinstance(idle, int) or not isinstance(heartbeat, int):
        return DEFAULT_SILENCE
    return float(max(idle, 1) + heartbeat)


async def _cancel(*tasks: asyncio.Future[Any]) -> None:
    """Cancels tasks and waits until they have finished.

    Args:
        *tasks: The tasks.
    """
    for task in tasks:
        task.cancel()
    await asyncio.wait(tasks)
    for task in tasks:
        if not task.cancelled():
            task.exception()


def _discard(connection: ClientConnection) -> None:
    """Drops a connection at once, without a close handshake.

    Args:
        connection: The connection.
    """
    transport = connection.transport
    if transport is not None:
        transport.abort()


class Engine:
    """The engine's state and its task.

    Attributes:
        stopped: True once the client has stopped for good.
        close_code: The close code that stopped it, if any.
    """

    def __init__(self, config: ClientConfig, events: EventHub) -> None:
        """Builds an engine that hasn't connected yet.

        Args:
            config: How to connect.
            events: Where events go.
        """
        self._config = config
        self._events = events
        self._commands: deque[_Command] = deque()
        self._wake = asyncio.Event()
        self._close_requested = asyncio.Event()
        self._topics: dict[str, _Topic] = {}
        self._patterns: dict[str, _Pattern] = {}
        self._unclaimed: set[str] = set()
        self._held = False
        self._subscribe_waiters: dict[str, asyncio.Future[dict[str, Any]]] = {}
        self._pending: dict[int, _Pending] = {}
        self._unconfirmed: dict[int, dict[str, Any]] = {}
        self._offline: deque[dict[str, Any]] = deque()
        self._next_id = 1
        self._connected_once = False
        self._silence = DEFAULT_SILENCE
        self.ready: asyncio.Future[None] = asyncio.get_running_loop().create_future()
        self.stopped = False
        self.close_code: int | None = None
        self._task: asyncio.Task[None] | None = None
        self._frame_handlers: dict[str, _FrameHandler] = {
            "message": self._on_message,
            "subscribed": self._on_subscribed,
            "unsubscribed": self._on_unsubscribed,
            "published": self._on_published,
            "truncated": self._on_truncated,
            "stalled": self._on_stalled,
            "auth_expiring": self._on_auth_expiring,
            "logged_in": self._on_logged_in,
            "authenticated": self._on_authenticated,
            "error": self._on_error,
        }

    def start(self) -> None:
        """Starts the engine's task."""
        self._task = asyncio.ensure_future(self._run())

    async def wait_stopped(self, timeout: float | None = None) -> None:
        """Waits until the engine's task has finished, cancelling it after a timeout.

        Args:
            timeout: Seconds to wait before cancelling the task; None waits
                as long as it takes.
        """
        if self._task is None:
            return
        done, _ = await asyncio.wait({self._task}, timeout=timeout)
        if not done:
            log.warning("the engine didn't stop in time, cancelling it")
            await _cancel(self._task)
        elif not self._task.cancelled():
            self._task.exception()

    def closed_error(self) -> ClosedError:
        """The error for calls after the client stopped.

        Returns:
            `ClosedError` with the close code that stopped it.
        """
        return ClosedError(self.close_code)

    def submit(self, command: _Command) -> None:
        """Queues a command for the engine.

        Args:
            command: The command.

        Raises:
            ClosedError: When the client has stopped.
        """
        if self.stopped:
            raise self.closed_error()
        self._commands.append(command)
        self._wake.set()

    def request_close(self) -> None:
        """Asks the engine to close the connection and stop."""
        if self.stopped:
            return
        self._close_requested.set()
        self._commands.append(_Close())
        self._wake.set()

    def ack(self, topic: str, seq: int) -> None:
        """Queues an acknowledgement; does nothing once stopped.

        Args:
            topic: The topic.
            seq: The message's seq.
        """
        if not self.stopped:
            self.submit(_Ack(topic, seq))

    def forget(self, topic: str, generation: int) -> None:
        """Queues an unsubscribe for a closed subscription; does nothing once stopped.

        Args:
            topic: The topic.
            generation: The subscription being closed.
        """
        if not self.stopped:
            self.submit(_Forget(topic, generation))

    async def _run(self) -> None:
        """Connects, serves, reconnects with backoff, and stops for good.

        Settles `ready` with the first connection's outcome.
        """
        attempt = 0
        last_error: PopSubError | None = None
        final_code: CloseCode | None = None
        try:
            while True:
                outcome = await self._connect_unless_closed()
                if outcome is None:
                    break
                if isinstance(outcome, _ConnectFailedError):
                    log.warning("connecting failed: %s", outcome.error.kind)
                    last_error = outcome.error
                    if not outcome.retry:
                        _settle(self.ready, error=outcome.error)
                        break
                else:
                    connection, resumed = outcome
                    attempt = 0
                    self._connected_once = True
                    _settle(self.ready)
                    self._events.emit(Connected(tuple(resumed)))
                    ending = await self._serve(connection, resumed)
                    if ending.stop:
                        break
                    _discard(connection)
                    self._fail_requests()
                    reconnecting = self._should_reconnect(ending.code)
                    code = int(ending.code) if ending.code is not None else None
                    self._events.emit(Disconnected(code, reconnecting))
                    if not reconnecting:
                        final_code = ending.code
                        break
                attempt += 1
                limit = self._config.reconnect.max_attempts
                if limit is not None and attempt > limit:
                    reason = f"; the last attempt failed: {last_error}" if last_error is not None else ""
                    exhausted = ConnectError(f"ran out of reconnect attempts{reason}")
                    exhausted.__cause__ = last_error
                    _settle(self.ready, error=exhausted)
                    break
                if not await self._wait_before_retry(attempt):
                    break
        finally:
            self._stop(final_code)

    def _should_reconnect(self, code: CloseCode | None) -> bool:
        """Decides whether to reconnect after a connection was lost.

        Args:
            code: The server's close code, if any.

        Returns:
            True to reconnect. `token_expired` allows it only with password
            credentials, which log in again.
        """
        if code is None:
            return True
        if code == CloseCode.TOKEN_EXPIRED:
            return isinstance(self._config.credentials, PasswordCredentials)
        return code.should_reconnect

    async def _connect_unless_closed(
        self,
    ) -> tuple[_Connection, list[str]] | _ConnectFailedError | None:
        """Connects, giving up at once if the application closes the client.

        Returns:
            The connection and resumed topics, the failure, or None when the
            application closed the client first.
        """
        attempt = asyncio.ensure_future(self._connect())
        closing = asyncio.ensure_future(self._close_requested.wait())
        try:
            await asyncio.wait({attempt, closing}, return_when=asyncio.FIRST_COMPLETED)
        finally:
            await _cancel(closing)
            if not attempt.done():
                await _cancel(attempt)
        if attempt.cancelled():
            return None
        error = attempt.exception()
        if isinstance(error, _ConnectFailedError):
            return error
        if error is not None:
            return _ConnectFailedError(ConnectError(str(error)), True, error)
        if self._close_requested.is_set():
            connection, _ = attempt.result()
            await self._close_gracefully(connection)
            return None
        return attempt.result()

    async def _wait_before_retry(self, attempt: int) -> bool:
        """Waits a jittered backoff before the next attempt, queueing commands meanwhile.

        Args:
            attempt: How many attempts in a row have failed.

        Returns:
            False when the application closed the client while waiting.
        """
        delay = self._config.reconnect.ceiling(attempt) * random.random()
        log.debug("waiting %.3f s before reconnecting", delay)
        until = time.monotonic() + delay
        while True:
            while self._commands:
                command = self._commands.popleft()
                if isinstance(command, _Close):
                    return False
                self._queue_offline(command)
            remaining = until - time.monotonic()
            if remaining <= 0:
                return True
            self._wake.clear()
            try:
                await asyncio.wait_for(self._wake.wait(), remaining)
            except asyncio.TimeoutError:
                return True

    def _fail_requests(self) -> None:
        """Settles what a lost connection leaves unanswered.

        Admin requests fail with `ConnectError`, since the server may or may
        not have carried them out. Unsubscribes are queued to be sent again
        and reported done, as they are while disconnected.
        """
        for request_id, pending in list(self._pending.items()):
            if isinstance(pending, _PendingRequest):
                del self._pending[request_id]
                _settle(
                    pending.reply,
                    error=ConnectError(
                        "the connection dropped before the reply; the request may or may not have been carried out"
                    ),
                )
            elif isinstance(pending, _PendingUnsubscribe):
                del self._pending[request_id]
                self._offline.append({"type": "unsubscribe", "topic": pending.topic})
                _settle(pending.reply)
            elif isinstance(pending, (_PendingSubscribe, _PendingRelogin, _PendingReauth)):
                del self._pending[request_id]

    def _take_id(self) -> int:
        """Takes a request id.

        Returns:
            A new id.
        """
        request_id = self._next_id
        self._next_id += 1
        return request_id

    def _quiet_id(self, frame: dict[str, Any]) -> int | None:
        """Takes a request id for a queued frame the engine sends on its own.

        An unsubscribe always gets one, so its reply can't be mistaken for
        the server ending the subscription, which it announces with no id.

        Args:
            frame: The frame.

        Returns:
            An id for an unsubscribe, otherwise None.
        """
        return self._take_id() if frame.get("type") == "unsubscribe" else None

    async def _connect(self) -> tuple[_Connection, list[str]]:
        """Opens a connection, logs in if needed, and authenticates.

        With a session id the `auth` frame carries `hold_resumed`. Whether
        the server answered with `held` is recorded for this connection;
        an absent `held` means it didn't.

        Returns:
            The connection and the topics the server restored for the session.

        Raises:
            _ConnectFailedError: With `retry` False for rejected credentials, an
                unsupported protocol, a bad session id or an invalid URL, and
                True otherwise.
        """
        config = self._config
        try:
            connection = await connect(
                config.url,
                create_connection=_Connection,
                ssl=config.ssl_context if config.url.startswith("wss://") else None,
                open_timeout=config.request_timeout,
                close_timeout=CLOSE_WAIT,
                ping_interval=None,
                max_size=MAX_FRAME_BYTES,
            )
        except InvalidURI as error:
            raise _ConnectFailedError(ConfigError(str(error)), False, error) from error
        except (OSError, asyncio.TimeoutError, WebSocketException) as error:
            raise _ConnectFailedError(ConnectError(str(error) or type(error).__name__), True, error) from error
        assert isinstance(connection, _Connection)
        try:
            credentials = config.credentials
            if isinstance(credentials, PasswordCredentials):
                reply = await self._exchange(
                    connection,
                    {"type": "login", "username": credentials.username, "password": credentials.password},
                )
                if reply.get("type") != "logged_in" or not isinstance(reply.get("token"), str):
                    raise _refusal(reply)
                token = reply["token"]
            else:
                token = credentials.token
            auth: dict[str, Any] = {"type": "auth", "token": token, "protocol": PROTOCOL_VERSION}
            if config.session_id is not None:
                auth["session_id"] = config.session_id
                auth["hold_resumed"] = True
            if config.clean_start and not self._connected_once:
                auth["clean_start"] = True
            reply = await self._exchange(connection, auth)
            if reply.get("type") != "authenticated":
                raise _refusal(reply)
        except BaseException:
            _discard(connection)
            raise
        self._silence = _silence_limit(reply.get("limits"))
        resumed = [topic for topic in reply.get("resumed") or [] if isinstance(topic, str)]
        self._held = reply.get("held") is True
        log.info("connected, %d topics resumed, held %s", len(resumed), self._held)
        return connection, resumed

    async def _exchange(self, connection: _Connection, frame: dict[str, Any]) -> dict[str, Any]:
        """Sends a request while connecting and waits for its reply.

        Args:
            connection: The connection.
            frame: The request, without an id.

        Returns:
            The reply frame.

        Raises:
            _ConnectFailedError: When the connection closes or drops first, or no
                reply arrives within the request timeout.
        """
        request_id = self._take_id()
        try:
            await connection.send(json.dumps({"id": request_id, **frame}))
        except ConnectionClosed as error:
            raise _ConnectFailedError(ConnectError("the connection dropped"), True, error) from error

        async def wait() -> dict[str, Any]:
            """Reads frames until the reply to this request arrives.

            Returns:
                The reply.

            Raises:
                _ConnectFailedError: When the connection closes or drops first.
            """
            while True:
                try:
                    data = await connection.recv()
                except ConnectionClosed as error:
                    code = _close_code(error)
                    if code is None and error.rcvd is None:
                        raise _ConnectFailedError(ConnectError("the connection closed"), True, error) from error
                    retry = code is None or code.should_reconnect
                    raise _ConnectFailedError(ClosedError(int(code) if code else None), retry, error) from error
                if not isinstance(data, str):
                    continue
                try:
                    reply = json.loads(data)
                except ValueError:
                    continue
                if isinstance(reply, dict) and reply.get("id") == request_id:
                    return reply

        try:
            return await asyncio.wait_for(wait(), self._config.request_timeout)
        except asyncio.TimeoutError as error:
            raise _ConnectFailedError(RequestTimeoutError("no reply while connecting"), True, error) from error

    async def _close_gracefully(self, connection: _Connection) -> None:
        """Closes a connection with a close handshake, bounded by `CLOSE_WAIT`.

        Args:
            connection: The connection.
        """
        try:
            await asyncio.wait_for(connection.close(), CLOSE_WAIT)
        except (asyncio.TimeoutError, OSError, WebSocketException):
            _discard(connection)

    async def _send(self, connection: _Connection, request_id: int | None, frame: dict[str, Any]) -> None:
        """Sends one frame.

        Args:
            connection: The connection.
            request_id: The request id, if a reply is expected.
            frame: The frame, without an id.

        Raises:
            _ConnectionLostError: When the connection is gone.
        """
        payload = frame if request_id is None else {"id": request_id, **frame}
        try:
            await connection.send(json.dumps(payload))
        except ConnectionClosed as error:
            raise _ConnectionLostError(_close_code(error)) from error
        except OSError as error:
            raise _ConnectionLostError(None) from error

    async def _serve(self, connection: _Connection, resumed: list[str]) -> _Ending:
        """Serves one connection until it ends.

        Works out which resumed topics are unclaimed, restores
        subscriptions, resends unconfirmed publishes and sends queued frames
        first, then handles commands and frames.

        Args:
            connection: The authenticated connection.
            resumed: Topics the server restored for the session.

        Returns:
            How the connection ended.
        """
        receiving: asyncio.Future[Any] | None = None
        waking: asyncio.Future[Any] | None = None
        try:
            self._note_resumed(resumed)
            await self._restore(connection, resumed)
            while True:
                if await self._run_commands(connection):
                    return _Ending(stop=True)
                remaining = connection.last_heard + self._silence - time.monotonic()
                if remaining <= 0:
                    log.warning("no word from the server, reconnecting")
                    return _Ending(stop=False)
                if receiving is None:
                    receiving = asyncio.ensure_future(connection.recv())
                waking = asyncio.ensure_future(self._wake.wait())
                done, _ = await asyncio.wait(
                    {receiving, waking}, timeout=remaining, return_when=asyncio.FIRST_COMPLETED
                )
                await _cancel(waking)
                waking = None
                if receiving in done:
                    finished, receiving = receiving, None
                    ending = await self._on_received(connection, finished)
                    if ending is not None:
                        return ending
        except _ConnectionLostError as lost:
            return _Ending(stop=False, code=lost.code)
        finally:
            leftover = [task for task in (receiving, waking) if task is not None]
            if leftover:
                await _cancel(*leftover)

    async def _run_commands(self, connection: _Connection) -> bool:
        """Acts on every queued command, stopping at a close.

        Args:
            connection: The connection.

        Returns:
            True when the application closed the client; the connection is
            then closed.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        while self._commands:
            command = self._commands.popleft()
            if isinstance(command, _Close):
                await self._close_gracefully(connection)
                return True
            await self._on_command(connection, command)
        self._wake.clear()
        return False

    async def _on_received(self, connection: _Connection, finished: asyncio.Future[Any]) -> _Ending | None:
        """Handles the outcome of one `recv`.

        Args:
            connection: The connection.
            finished: The completed `recv` task.

        Returns:
            How the connection ended, or None to keep serving it. Binary and
            malformed frames are ignored.

        Raises:
            _ConnectionLostError: When a write fails while acting on the frame.
        """
        try:
            data = finished.result()
        except ConnectionClosed as error:
            return _Ending(stop=False, code=_close_code(error))
        except (OSError, WebSocketException):
            return _Ending(stop=False)
        if not isinstance(data, str):
            return None
        try:
            frame = json.loads(data)
        except ValueError:
            log.debug("ignoring a frame that isn't JSON")
            return None
        if isinstance(frame, dict):
            await self._on_frame(connection, frame)
        return None

    def _note_resumed(self, resumed: list[str]) -> None:
        """Records which of a new connection's resumed topics are unclaimed.

        Those are the topics the server restored for the session that the
        engine isn't tracking: the application hasn't subscribed to them in
        this process. Their messages are dropped without an acknowledgement
        until the application claims them, so the server keeps them.

        When the server holds the session's topics (`held`) it sends nothing
        on them until the engine subscribes, so none is unclaimed.

        Args:
            resumed: Topics the server restored.
        """
        self._unclaimed = set() if self._held else {name for name in resumed if name not in self._topics}

    async def _restore(self, connection: _Connection, resumed: list[str]) -> None:
        """Restores state on a new connection.

        When the server holds the session's topics (`held`), each tracked
        topic it lists as resumed is subscribed again from `resume`, which
        is one past the session's cursor, since the server no longer
        attaches it. Otherwise
        topics the server restored are left to it, except one still waiting
        for the `subscribed` that claims it, which is subscribed again: from
        the application's start, or from `resume` when a pattern owns it.
        Every other tracked topic is subscribed again from the next seq it
        expects. A shared
        subscription always rejoins its group with `resume`, since it is
        never restored and its position is on the server. Wildcard patterns
        are sent after the topics, from seq 1: the server skips topics the
        connection holds, so that joins only topics created while it was
        away, from their first message. Then unconfirmed publishes are
        resent in their original order with their original request ids, and
        queued frames are sent.

        Args:
            connection: The connection.
            resumed: Topics the server restored.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        again: list[dict[str, Any]] = []
        for name, topic in self._topics.items():
            if topic.group is not None:
                again.append({"type": "subscribe", "topic": name, "from": "resume", "group": topic.group})
            elif topic.claiming:
                start = StartFrom.resume() if topic.owner is not None else topic.state.start
                again.append({"type": "subscribe", "topic": name, "from": start.to_json()})
            elif name not in resumed:
                again.append({"type": "subscribe", "topic": name, "from": topic.state.resubscribe_from().to_json()})
            elif self._held:
                again.append({"type": "subscribe", "topic": name, "from": StartFrom.resume().to_json()})
        again.extend(
            {"type": "subscribe", "topic": pattern, "from": StartFrom.seq(1).to_json()}
            for pattern in sorted(self._patterns)
        )
        for frame in again:
            request_id = self._take_id()
            self._pending[request_id] = _PendingSubscribe(str(frame["topic"]))
            await self._send(connection, request_id, frame)
        for request_id in sorted(self._unconfirmed):
            unconfirmed = self._unconfirmed.get(request_id)
            if unconfirmed is not None:
                await self._send(connection, request_id, unconfirmed)
        await self._flush_offline(connection)

    async def _flush_offline(self, connection: _Connection) -> None:
        """Sends every queued frame, oldest first.

        Args:
            connection: The connection.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        while self._offline:
            frame = self._offline.popleft()
            await self._send(connection, self._quiet_id(frame), frame)

    def _queue_offline(self, command: _Command) -> None:
        """Records a command that arrived while disconnected, to act on after reconnecting.

        Args:
            command: The command.
        """
        if isinstance(command, _Subscribe):
            self._add_topic(command)
        elif isinstance(command, _Unsubscribe):
            for name in self._release(command.topic, None) or [command.topic]:
                self._offline.append({"type": "unsubscribe", "topic": name})
            _settle(command.reply)
        elif isinstance(command, _Forget):
            for name in self._release(command.topic, command.generation):
                self._offline.append({"type": "unsubscribe", "topic": name})
        elif isinstance(command, _Publish):
            self._queue_publish(command)
        elif isinstance(command, _Ack):
            topic = self._topics.get(command.topic)
            if topic is not None:
                topic.state.acked.insert(command.seq)
        elif isinstance(command, _Request):
            _settle(
                command.reply,
                error=ConnectError("not connected; admin requests aren't queued while disconnected"),
            )

    def _queue_publish(self, command: _Publish) -> int | None:
        """Registers a publish until it is sent or confirmed.

        A confirmed one waits in `unconfirmed` for its `published`; a QoS 0
        one waits in the offline queue until it is sent.

        Args:
            command: The publish.

        Returns:
            The request id of a confirmed publish, otherwise None.
        """
        if command.reply is not None and command.message_id is not None:
            request_id = self._take_id()
            self._pending[request_id] = _PendingPublish(command.reply)
            self._unconfirmed[request_id] = {
                "type": "publish",
                "topic": command.topic,
                "payload": command.payload,
                "message_id": command.message_id,
                "qos": 1,
            }
            return request_id
        if len(self._offline) >= OFFLINE_PUBLISHES:
            self._offline.popleft()
            log.warning("dropped the oldest publish held while disconnected")
        self._offline.append({"type": "publish", "topic": command.topic, "payload": command.payload})
        return None

    def _add_topic(self, command: _Subscribe) -> list[str]:
        """Adds or replaces a subscription and registers the subscribe's waiter.

        A wildcard pattern is recorded as a pattern; anything else as a
        topic subscribed by name. A subscription it replaces ends, and an
        earlier subscribe still waiting on the same name fails. A topic
        subscribed again by name starts over from the subscribe's start.

        A topic subscribed by name that is unclaimed is claimed: it is
        tracked with `claiming` set, so what the server sent on its own is
        dropped until the `subscribed` that answers this subscribe. A topic
        subscribed again while its claim is unanswered stays `claiming`.

        Args:
            command: The subscribe.

        Returns:
            For a pattern, the session's topics it claimed, which the caller
            subscribes to by name from `resume`. Empty for a topic.
        """
        claimed: list[str] = []
        if is_pattern(command.topic):
            claimed = self._add_pattern(command)
        else:
            held = self._topics.get(command.topic)
            claiming = command.topic in self._unclaimed or (held is not None and held.claiming)
            self._release(command.topic, None)
            self._remove_topic(command.topic)
            self._unclaimed.discard(command.topic)
            state = TopicState(command.start, shared=command.group is not None)
            self._topics[command.topic] = _Topic(
                state, command.channel, command.generation, group=command.group, claiming=claiming
            )
        earlier = self._subscribe_waiters.pop(command.topic, None)
        if earlier is not None:
            _settle(earlier, error=ProtocolError("replaced by a later subscribe to the same topic"))
        self._subscribe_waiters[command.topic] = command.reply
        return claimed

    def _add_pattern(self, command: _Subscribe) -> list[str]:
        """Records a wildcard subscription, replacing one the engine holds.

        When the pattern is already held, the topics it owned move to the
        new subscription and keep their deduplication and acknowledgement
        state, because the server sends nothing again for topics the
        connection holds. The earlier subscription ends, and closing it
        later releases nothing, since its generation no longer matches.

        Every unclaimed topic the pattern matches is claimed: tracked as
        owned by the pattern with `claiming` set. The server skips topics
        the connection holds when it handles the pattern, so the caller
        subscribes to each by name to have its messages replayed.

        Args:
            command: The subscribe, whose topic is a pattern.

        Returns:
            The claimed topics, sorted.
        """
        pattern = command.topic
        earlier = self._patterns.get(pattern)
        if earlier is not None and earlier.channel is not command.channel:
            earlier.channel.close_sender()
        for held in self._topics.values():
            if held.owner == pattern:
                held.channel = command.channel
                held.generation = command.generation
        claimed = sorted(name for name in self._unclaimed if matches_pattern(pattern, name))
        for name in claimed:
            self._unclaimed.discard(name)
            self._topics[name] = _Topic(
                TopicState(StartFrom.resume()), command.channel, command.generation, owner=pattern, claiming=True
            )
        self._patterns[pattern] = _Pattern(command.channel, command.generation)
        return claimed

    def _adopt(self, name: str, start: int | None) -> bool:
        """Starts tracking a topic the server joined for a wildcard subscription.

        When several patterns match, the oldest owns it.

        Args:
            name: The topic.
            start: The first seq the server will send, when known.

        Returns:
            Whether a pattern matched.
        """
        owners = [(pattern, held) for pattern, held in self._patterns.items() if matches_pattern(pattern, name)]
        if not owners:
            return False
        pattern, held = min(owners, key=lambda owner: owner[1].generation)
        state = TopicState(StartFrom.seq(start if start is not None else 1), next_expected=start)
        self._topics[name] = _Topic(state, held.channel, held.generation, owner=pattern)
        return True

    def _release(self, name: str, generation: int | None) -> list[str]:
        """Stops tracking a subscription and ends it for the application.

        That is a topic subscribed by name, or a wildcard pattern together
        with every topic joined through it. A topic owned by a pattern is not
        released under its own name.

        Args:
            name: The topic or pattern.
            generation: Only release it if it is still this subscription;
                None releases whatever is there.

        Returns:
            What to unsubscribe from on the server: the name, and for a
            pattern the topics it owned. Empty when nothing was released.
        """
        pattern = self._patterns.get(name)
        if pattern is not None and generation in (None, pattern.generation):
            del self._patterns[name]
            pattern.channel.close_sender()
            owned = [topic for topic, held in self._topics.items() if held.owner == name]
            for topic in owned:
                del self._topics[topic]
            return [name, *owned]
        held = self._topics.get(name)
        if held is not None and held.owner is None and generation in (None, held.generation):
            del self._topics[name]
            held.channel.close_sender()
            return [name]
        return []

    def _remove_topic(self, name: str) -> bool:
        """Stops tracking one topic.

        A topic subscribed by name ends its subscription. A topic owned by a
        pattern only stops; the pattern's subscription carries on.

        Args:
            name: The topic.

        Returns:
            Whether it was tracked.
        """
        topic = self._topics.pop(name, None)
        if topic is None:
            return False
        if topic.owner is None:
            topic.channel.close_sender()
        return True

    async def _on_command(self, connection: _Connection, command: _Command) -> None:
        """Acts on a command while connected.

        Args:
            connection: The connection.
            command: The command.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        if isinstance(command, _Subscribe):
            await self._send_subscribe(connection, command)
        elif isinstance(command, _Unsubscribe):
            await self._send_unsubscribe(connection, command)
        elif isinstance(command, _Forget):
            for name in self._release(command.topic, command.generation):
                await self._send(connection, self._take_id(), {"type": "unsubscribe", "topic": name})
        elif isinstance(command, _Publish):
            await self._send_publish(connection, command)
        elif isinstance(command, _Ack):
            self._remember_ack(command)
            await self._send(connection, None, {"type": "ack", "topic": command.topic, "seq": command.seq})
        elif isinstance(command, _Request):
            request_id = self._take_id()
            self._pending[request_id] = _PendingRequest(command.reply)
            await self._send(connection, request_id, command.frame)

    async def _send_subscribe(self, connection: _Connection, command: _Subscribe) -> None:
        """Registers a subscribe and sends it.

        A pattern that claimed topics of the session is followed by one
        `subscribe` by name from `resume` for each of them.

        Args:
            connection: The connection.
            command: The subscribe.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        claimed = self._add_topic(command)
        request_id = self._take_id()
        self._pending[request_id] = _PendingSubscribe(command.topic)
        frame: dict[str, Any] = {"type": "subscribe", "topic": command.topic, "from": command.start.to_json()}
        if command.group is not None:
            frame["group"] = command.group
        await self._send(connection, request_id, frame)
        for name in claimed:
            request_id = self._take_id()
            self._pending[request_id] = _PendingSubscribe(name)
            claim = {"type": "subscribe", "topic": name, "from": StartFrom.resume().to_json()}
            await self._send(connection, request_id, claim)

    async def _send_unsubscribe(self, connection: _Connection, command: _Unsubscribe) -> None:
        """Releases a subscription and unsubscribes on the server.

        A pattern sends one `unsubscribe` for itself and one for each topic
        it owned; the first reply answers the caller. Every frame carries an
        id, so no reply is mistaken for the server ending a subscription.

        Args:
            connection: The connection.
            command: The unsubscribe.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        names = self._release(command.topic, None) or [command.topic]
        for index, name in enumerate(names):
            request_id = self._take_id()
            if index == 0:
                self._pending[request_id] = _PendingUnsubscribe(name, command.reply)
            await self._send(connection, request_id, {"type": "unsubscribe", "topic": name})

    async def _send_publish(self, connection: _Connection, command: _Publish) -> None:
        """Registers a publish and sends it, after any QoS 0 publishes queued before it.

        Args:
            connection: The connection.
            command: The publish.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        request_id = self._queue_publish(command)
        if request_id is None:
            await self._flush_offline(connection)
        else:
            await self._send(connection, request_id, self._unconfirmed[request_id])

    def _remember_ack(self, command: _Ack) -> None:
        """Records an acknowledgement so a redelivered copy is acknowledged again.

        Args:
            command: The acknowledgement.
        """
        topic = self._topics.get(command.topic)
        if topic is not None:
            topic.state.acked.insert(command.seq)

    async def _on_frame(self, connection: _Connection, frame: dict[str, Any]) -> None:
        """Acts on a frame from the server.

        A reply to an admin request goes straight to its caller. Every other
        frame goes to the handler for its `type`; unknown types are ignored,
        as the protocol asks.

        Args:
            connection: The connection.
            frame: The frame.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        frame_id = frame.get("id")
        pending: _Pending | None = None
        if isinstance(frame_id, int) and not isinstance(frame_id, bool):
            pending = self._pending.pop(frame_id, None)
        if isinstance(pending, _PendingRequest):
            if frame.get("type") == "error":
                _settle(pending.reply, error=_server_error(frame))
            else:
                _settle(pending.reply, frame)
            return
        handler = self._frame_handlers.get(str(frame.get("type")))
        if handler is not None:
            await handler(connection, frame, pending)

    async def _on_message(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Handles a `message` frame.

        Args:
            connection: The connection.
            frame: The frame.
            pending: Unused; deliveries answer no request.

        Raises:
            _ConnectionLostError: When an acknowledgement can't be written.
        """
        await self._deliver(connection, frame)

    async def _on_subscribed(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Records where a subscription starts and answers its subscribe.

        For a topic being claimed, this is the answer to the claim: the
        topic stops dropping messages and its next expected seq becomes
        `replay_from`, whatever it was.

        Args:
            connection: Unused.
            frame: The `subscribed` frame.
            pending: Unused; the waiter is found by topic, since a session's
                restored subscriptions arrive without an id.
        """
        name = str(frame.get("topic", ""))
        replay_from = frame.get("replay_from")
        start = replay_from if isinstance(replay_from, int) else None
        topic = self._topics.get(name)
        if name in self._patterns:
            log.debug("wildcard subscription accepted")
        elif topic is None:
            self._adopt(name, start)
        else:
            if topic.claiming:
                topic.claiming = False
                topic.state.next_expected = None
            if start is not None:
                topic.state.advance(start)
        waiter = self._subscribe_waiters.pop(name, None)
        if waiter is not None:
            _settle(waiter, frame)

    async def _on_unsubscribed(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Answers an unsubscribe, or ends a subscription the server ended.

        Args:
            connection: Unused.
            frame: The `unsubscribed` frame.
            pending: The unsubscribe it answers, if any. With no id at all,
                the server ended the subscription on its own.
        """
        name = str(frame.get("topic", ""))
        if isinstance(pending, _PendingUnsubscribe):
            _settle(pending.reply)
        elif frame.get("id") is None and self._remove_topic(name):
            log.warning("the server ended a subscription")
            self._events.emit(Unsubscribed(name))

    async def _on_published(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Confirms a confirmed publish.

        Args:
            connection: Unused.
            frame: The `published` frame.
            pending: The publish it answers, if any.
        """
        frame_id = frame.get("id")
        if isinstance(pending, _PendingPublish) and isinstance(frame_id, int):
            self._unconfirmed.pop(frame_id, None)
            _settle(pending.reply, Published(int(frame["seq"]), str(frame["message_id"])))

    async def _on_truncated(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Moves a topic's next expected seq past trimmed history and reports it.

        Args:
            connection: Unused.
            frame: The `truncated` frame.
            pending: Unused.
        """
        name = str(frame.get("topic", ""))
        first = int(frame.get("first_available", 0))
        topic = self._topics.get(name)
        if topic is not None:
            topic.state.advance(first)
        self._events.emit(Truncated(name, first))

    async def _on_stalled(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Reports a message that ran out of retries.

        Args:
            connection: Unused.
            frame: The `stalled` frame.
            pending: Unused.
        """
        self._events.emit(Stalled(str(frame.get("topic", "")), int(frame.get("seq", 0))))

    async def _on_auth_expiring(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Reports the coming expiry and, with password credentials, logs in again.

        Args:
            connection: The connection.
            frame: The `auth_expiring` frame.
            pending: Unused.

        Raises:
            _ConnectionLostError: When the login can't be written.
        """
        self._events.emit(AuthExpiring(int(frame.get("expires_at_ms", 0))))
        credentials = self._config.credentials
        if isinstance(credentials, PasswordCredentials):
            request_id = self._take_id()
            self._pending[request_id] = _PendingRelogin()
            login = {"type": "login", "username": credentials.username, "password": credentials.password}
            await self._send(connection, request_id, login)

    async def _on_logged_in(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Installs the token from a refresh login with a new `auth`.

        Args:
            connection: The connection.
            frame: The `logged_in` frame.
            pending: The refresh login it answers, if any.

        Raises:
            _ConnectionLostError: When the `auth` can't be written.
        """
        token = frame.get("token")
        if isinstance(pending, _PendingRelogin) and isinstance(token, str):
            request_id = self._take_id()
            self._pending[request_id] = _PendingReauth()
            await self._send(connection, request_id, {"type": "auth", "token": token, "protocol": PROTOCOL_VERSION})

    async def _on_authenticated(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Reports a refreshed token.

        Args:
            connection: Unused.
            frame: The `authenticated` frame.
            pending: The refresh `auth` it answers, if any.
        """
        if isinstance(pending, _PendingReauth):
            self._events.emit(AuthRefreshed())

    async def _on_error(self, connection: _Connection, frame: dict[str, Any], pending: _Pending | None) -> None:
        """Routes an `error` frame to the request it answers, or to the events.

        Args:
            connection: Unused.
            frame: The frame.
            pending: The request it answers, if its id matched one.
        """
        error = _server_error(frame)
        waiter: asyncio.Future[Any] | None = None
        if isinstance(pending, _PendingSubscribe):
            self._release(pending.topic, None)
            waiter = self._subscribe_waiters.pop(pending.topic, None)
        elif isinstance(pending, (_PendingUnsubscribe, _PendingPublish)):
            waiter = pending.reply
            frame_id = frame.get("id")
            if isinstance(frame_id, int):
                self._unconfirmed.pop(frame_id, None)
        if waiter is None:
            self._events.emit(ServerErrorEvent(error.code, error.message))
        else:
            _settle(waiter, error=error)

    async def _deliver(self, connection: _Connection, frame: dict[str, Any]) -> None:
        """Hands a delivered message to its subscription.

        Drops duplicates and acknowledges as the acknowledgement mode says.

        A message on an unclaimed topic, or on a topic whose claim is still
        unanswered, is dropped without an acknowledgement: the server keeps
        it and replays it when the application's subscribe is handled.

        Waits while the subscription's buffer is full, which stops the
        engine reading the socket until the application catches up or the
        client is closed.

        Args:
            connection: The connection, for acknowledgements.
            frame: The `message` frame.

        Raises:
            _ConnectionLostError: When a write fails.
        """
        name = str(frame.get("topic", ""))
        seq = int(frame.get("seq", 0))
        qos = int(frame.get("qos", 0))
        ack = {"type": "ack", "topic": name, "seq": seq}
        if name in self._unclaimed:
            return
        if name not in self._topics:
            self._adopt(name, None)
        topic = self._topics.get(name)
        if topic is None:
            if qos == 1:
                await self._send(connection, None, ack)
            return
        if topic.claiming:
            return
        verdict = topic.state.accept(seq, qos, self._config.ack_mode)
        if verdict.ack:
            await self._send(connection, None, ack)
        if not verdict.deliver:
            return
        manual = qos == 1 and self._config.ack_mode == AckMode.MANUAL
        message = Message(
            topic=name,
            seq=seq,
            message_id=str(frame.get("message_id", "")),
            payload=str(frame.get("payload", "")),
            qos=qos,
            published_at_ms=int(frame.get("published_at_ms", 0)),
            _engine=self if manual else None,
        )
        delivered = await topic.channel.put(message, self._close_requested)
        if not delivered and not self._close_requested.is_set():
            for released in self._release(topic.owner or name, topic.generation):
                await self._send(connection, self._take_id(), {"type": "unsubscribe", "topic": released})

    def _stop(self, code: CloseCode | None) -> None:
        """Stops the client for good: records why and fails everything waiting.

        Args:
            code: The close code that stopped it, if any.
        """
        self.stopped = True
        self.close_code = int(code) if code is not None else None
        error = self.closed_error()
        for waiter in self._subscribe_waiters.values():
            _settle(waiter, error=error)
        self._subscribe_waiters.clear()
        for pending in self._pending.values():
            if isinstance(pending, (_PendingUnsubscribe, _PendingPublish, _PendingRequest)):
                _settle(pending.reply, error=error)
        self._pending.clear()
        self._unconfirmed.clear()
        for name in [*self._patterns, *self._topics]:
            self._release(name, None)
        self._topics.clear()
        while self._commands:
            command = self._commands.popleft()
            reply = getattr(command, "reply", None)
            if isinstance(reply, asyncio.Future):
                _settle(reply, error=error)
        _settle(self.ready, error=error)
        self._events.end()
        log.info("client stopped (close code %s)", self.close_code)
