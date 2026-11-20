"""`Client`, the handle the application holds."""

from __future__ import annotations

import asyncio
import itertools
import uuid
from typing import Any, TypeVar

from popsub._engine import STOP_WAIT, Engine, _Publish, _Request, _Subscribe, _Unsubscribe
from popsub._state import Channel
from popsub.admin import Admin
from popsub.config import ClientConfig
from popsub.errors import PopSubError, RequestTimeoutError
from popsub.message import EventHub, EventStream, Message, Published, Subscription
from popsub.protocol import StartFrom

T = TypeVar("T")


class Client:
    """A connection to a PopSub server that reconnects on its own.

    Create one with `await Client.connect(config)`. It stays connected until
    `close` is called or the server stops it for good (see `ClosedError`).
    It can be used as an async context manager, which closes it on exit.
    """

    def __init__(self, config: ClientConfig, engine: Engine, events: EventHub) -> None:
        """Wraps a started engine. Use `Client.connect` instead.

        Args:
            config: The configuration.
            engine: The engine that owns the connection.
            events: Where the engine sends events.
        """
        self._config = config
        self._engine = engine
        self._events = events
        self._generations = itertools.count(1)

    @classmethod
    async def connect(cls, config: ClientConfig) -> Client:
        """Connects to a server and starts the client.

        Waits for the first connection to log in and authenticate, retrying
        with the reconnect policy, for up to `config.request_timeout`.

        Args:
            config: Where and how to connect.

        Returns:
            The connected client.

        Raises:
            ConfigError: When the configuration is invalid.
            ServerError: When the server refused the credentials, the
                protocol version or the session id (`invalid_credentials`,
                for example). These are not retried.
            RequestTimeoutError: When no connection succeeded in time.
            ConnectError: When the reconnect policy ran out of attempts.
            ClosedError: When the server closed the connection for good
                while connecting.
        """
        config.validate()
        events = EventHub()
        engine = Engine(config, events)
        engine.start()
        client = cls(config, engine, events)
        try:
            await asyncio.wait_for(asyncio.shield(engine.ready), config.request_timeout)
        except asyncio.TimeoutError as error:
            await client.close()
            if engine.ready.done() and not engine.ready.cancelled():
                engine.ready.exception()
            raise RequestTimeoutError("no connection succeeded within the request timeout") from error
        except PopSubError:
            await engine.wait_stopped()
            raise
        return client

    @property
    def config(self) -> ClientConfig:
        """The configuration the client was created with.

        Returns:
            The configuration.
        """
        return self._config

    @property
    def stopped(self) -> bool:
        """Says whether the client has stopped for good.

        Returns:
            True once stopped.
        """
        return self._engine.stopped

    @property
    def close_code(self) -> int | None:
        """The close code that stopped the client.

        Returns:
            The code, or None while running and when it stopped without one
            (closed by the application, or out of reconnect attempts).
        """
        return self._engine.close_code

    async def subscribe(self, topic: str, start_from: StartFrom | None = None) -> Subscription:
        """Subscribes to a topic, or to every topic matching a wildcard pattern.

        A pattern has `+` for any one level and a trailing `#` for the rest
        (`orders/#`, `sensors/+/temp`). Its subscription yields the messages
        of every matching topic the user may read, each under its own topic
        and seq, including topics created later.

        Subscribing to a topic or pattern already subscribed replaces the
        earlier subscription, which ends. A topic starts over from
        `start_from`. A pattern's joined topics move to the new subscription
        and carry on from the next seq each expects, so no message is
        repeated or lost.

        On a durable session held by an earlier process, messages the server
        kept for a topic are neither delivered nor acknowledged until this
        is called for the topic or for a pattern matching it; they are then
        replayed in order.

        Args:
            topic: The topic or pattern.
            start_from: Where replay starts. Defaults to `StartFrom.resume()`,
                which continues a durable session's stored subscription and
                replays nothing otherwise. For a pattern it applies to each
                topic that exists when subscribing.

        Returns:
            The subscription. A topic's messages arrive in seq order.

        Raises:
            ServerError: When the server refused it, for example
                `not_authorized` or `invalid_topic`.
            RequestTimeoutError: When it didn't complete in time.
            ClosedError: When the client has stopped.
        """
        return await self._subscribe(topic, start_from, None)

    async def subscribe_shared(self, topic: str, group: str, start_from: StartFrom | None = None) -> Subscription:
        """Joins a shared subscription: the group's members split the topic's messages.

        Each message goes to one member, so this subscription yields only
        some of them, not in strict seq order. A message can reach a second
        member after the first disconnects without acknowledging it. The
        group's position is kept on the server.

        Args:
            topic: The topic; a pattern is refused with `bad_request`.
            group: The group, 1 to 64 letters, digits, `_`, `.` or `-`.
            start_from: Where the group starts the first time it is used on
                the topic (`resume`, the default, then means `new`). Ignored
                when the group already has a position.

        Returns:
            The subscription.

        Raises:
            ServerError: When the server refused it, for example
                `not_authorized`, `invalid_topic` or `bad_request`.
            RequestTimeoutError: When it didn't complete in time.
            ClosedError: When the client has stopped.
        """
        return await self._subscribe(topic, start_from, group)

    async def _subscribe(self, topic: str, start_from: StartFrom | None, group: str | None) -> Subscription:
        """Subscribes to a topic or pattern, alone or as a member of a group.

        Args:
            topic: The topic or pattern.
            start_from: Where replay starts; None means `resume`.
            group: The shared subscription to join, if any.

        Returns:
            The subscription.

        Raises:
            ServerError: When the server refused it.
            RequestTimeoutError: When it didn't complete in time.
            ClosedError: When the client has stopped.
        """
        start = start_from if start_from is not None else StartFrom.resume()
        generation = next(self._generations)
        channel: Channel[Message] = Channel(self._config.subscription_buffer)
        reply: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self._engine.submit(_Subscribe(topic, start, generation, channel, reply, group))
        subscription = Subscription(topic, channel, self._engine, generation)
        try:
            subscribed = await self._wait(reply)
        except BaseException:
            subscription.close()
            raise
        replay_from = subscribed.get("replay_from")
        replay_to = subscribed.get("replay_to")
        subscription.replay_from = replay_from if isinstance(replay_from, int) else None
        subscription.replay_to = replay_to if isinstance(replay_to, int) else None
        return subscription

    async def unsubscribe(self, topic: str) -> None:
        """Unsubscribes from a topic; its subscription ends.

        A durable session forgets the stored subscription too.

        While disconnected, the unsubscribe is queued, sent after
        reconnecting, and this returns at once.

        Args:
            topic: The topic.

        Raises:
            ServerError: When the session store couldn't be updated.
            RequestTimeoutError: When it didn't complete in time.
            ClosedError: When the client has stopped.
        """
        reply: asyncio.Future[None] = asyncio.get_running_loop().create_future()
        self._engine.submit(_Unsubscribe(topic, reply))
        await self._wait(reply)

    async def publish(self, topic: str, payload: str) -> None:
        """Publishes a message at QoS 0: sent once, with no confirmation.

        Returns once the client has queued it. While disconnected, up to
        10,000 such publishes are held and sent after reconnecting; beyond
        that the oldest are dropped. Failures the server reports arrive as
        `ServerErrorEvent`.

        Args:
            topic: The topic.
            payload: The body.

        Raises:
            ClosedError: When the client has stopped.
        """
        self._engine.submit(_Publish(topic, payload))

    async def publish_confirmed(self, topic: str, payload: str) -> Published:
        """Publishes a message at QoS 1 and waits until the server has stored it.

        The message gets a new UUID as its `message_id`, and is resent with
        the same request id and `message_id` after a reconnect until it is
        confirmed, so the server stores it once.

        Args:
            topic: The topic.
            payload: The body.

        Returns:
            Its seq and message_id.

        Raises:
            ServerError: When the server refused it, for example
                `not_authorized`, `rate_limited` or `payload_too_large`.
            RequestTimeoutError: When it wasn't confirmed in time. It may
                still have been stored.
            ClosedError: When the client has stopped.
        """
        reply: asyncio.Future[Published] = asyncio.get_running_loop().create_future()
        self._engine.submit(_Publish(topic, payload, str(uuid.uuid4()), reply))
        return await self._wait(reply)

    def admin(self) -> Admin:
        """The admin requests: accounts, grants, disabling and deleting.

        They need the client to be logged in as an enabled admin.

        Returns:
            A handle that sends them through this client.
        """
        return Admin(self)

    def events(self) -> EventStream:
        """Opens a stream of the client's events from now on.

        Events are connects, disconnects, truncations, stalls, token
        refreshes, subscriptions the server ended, and errors that answer no
        request.

        Returns:
            The stream, an async iterator that ends after the client stops.
        """
        return self._events.stream()

    async def close(self) -> None:
        """Closes the connection and stops the client.

        Pending requests fail with `ClosedError` and subscriptions end. Every
        task the client started has finished when this returns: the engine
        gets `STOP_WAIT` seconds to close the connection before its task is
        cancelled. Does nothing if the client has already stopped.
        """
        self._engine.request_close()
        await self._engine.wait_stopped(STOP_WAIT)

    async def __aenter__(self) -> Client:
        """Enters an `async with` block.

        Returns:
            The client.
        """
        return self

    async def __aexit__(self, *exc_info: object) -> None:
        """Closes the client when the `async with` block ends.

        Args:
            *exc_info: The exception that ended the block, if any.
        """
        await self.close()

    async def _request(self, frame: dict[str, Any]) -> dict[str, Any]:
        """Sends an admin request and waits for its reply.

        It is not queued while disconnected and not resent after a reconnect.

        Args:
            frame: The request, without an id.

        Returns:
            The reply frame.

        Raises:
            ServerError: When the reply is an `error` frame.
            ConnectError: When disconnected, or the connection dropped first.
            RequestTimeoutError: When no reply arrived in time.
            ClosedError: When the client has stopped.
        """
        reply: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self._engine.submit(_Request(frame, reply))
        return await self._wait(reply)

    async def _wait(self, reply: asyncio.Future[T]) -> T:
        """Waits for a request's outcome, up to the request timeout.

        Args:
            reply: Where the engine puts the outcome.

        Returns:
            The outcome.

        Raises:
            RequestTimeoutError: When it didn't arrive in time.
            PopSubError: Whatever error the engine settled it with.
        """
        try:
            return await asyncio.wait_for(reply, self._config.request_timeout)
        except asyncio.TimeoutError as error:
            raise RequestTimeoutError() from error
