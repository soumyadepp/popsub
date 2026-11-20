"""What the application receives: messages, subscriptions and events.

A message's `payload` is the string that travelled. `Message.data` gives its
bytes: the decoded payload when `encoding` is base64, otherwise the payload's
UTF-8.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
from collections import deque
from dataclasses import dataclass, field
from typing import Any, ClassVar, Protocol

from popsub._state import Channel
from popsub.errors import ProtocolError
from popsub.protocol import Encoding

EVENT_BUFFER = 256
"""How many events an `EventStream` holds before dropping the oldest."""


class _Engine(Protocol):
    """The parts of the connection engine that messages and subscriptions use."""

    def ack(self, topic: str, seq: int) -> None:
        """Queues an acknowledgement.

        Args:
            topic: The topic.
            seq: The message's seq.
        """

    def forget(self, topic: str, generation: int) -> None:
        """Unsubscribes a subscription the application stopped listening to.

        Args:
            topic: The topic.
            generation: Which subscription it was.
        """


@dataclass(frozen=True)
class Published:
    """The result of a confirmed publish.

    Attributes:
        seq: The message's position in the topic.
        message_id: The id it was stored under.
    """

    seq: int
    message_id: str


@dataclass
class Message:
    """A message delivered on a subscription.

    Each `(topic, seq)` reaches the application at most once per client.

    Attributes:
        topic: The topic it was published to.
        seq: Its position in the topic.
        message_id: The publisher's id for it.
        payload: The body as it travelled: the text itself, or base64 when
            `encoding` says so. `data` gives the bytes either way.
        qos: 0 or 1.
        published_at_ms: When it was published, in milliseconds since the
            Unix epoch.
        encoding: How `payload` encodes the message's bytes. None means it
            is text; `Encoding.BASE64` means it was published as bytes.
    """

    topic: str
    seq: int
    message_id: str
    payload: str
    qos: int
    published_at_ms: int
    encoding: Encoding | None = None
    _engine: _Engine | None = field(default=None, repr=False, compare=False)

    @property
    def data(self) -> bytes:
        """The message's bytes.

        Returns:
            The payload's UTF-8 bytes for a text message, or the decoded
            payload for one published as bytes.

        Raises:
            ProtocolError: When the payload says it is base64 and isn't. The
                server checks this on publish, so it means the record was
                stored some other way.
        """
        if self.encoding is None:
            return self.payload.encode("utf-8")
        try:
            return base64.b64decode(self.payload, validate=True)
        except (binascii.Error, ValueError) as error:
            raise ProtocolError(f"the payload isn't valid base64: {error}") from error

    def ack(self) -> None:
        """Acknowledges the message.

        The server then stops resending it, and a durable session's cursor
        can move past it.

        Does nothing for QoS 0 messages, in automatic mode (the client
        already acknowledged it) and after the client stopped. Calling it more
        than once is harmless. While disconnected, the acknowledgement is
        remembered so a redelivered copy is acknowledged after reconnecting.
        """
        if self._engine is not None:
            self._engine.ack(self.topic, self.seq)


class Subscription:
    """The messages of one subscribed topic, in seq order.

    Iterate it with `async for`, or call `next`. Iteration ends when the
    server ends the subscription (see `Unsubscribed`), when the application
    unsubscribes or subscribes to the topic again, or when the client stops.

    Attributes:
        replay_from: The first seq of replayed history, from `subscribed`.
        replay_to: The last seq of replayed history; later ones are live.
    """

    def __init__(self, topic: str, channel: Channel[Message], engine: _Engine, generation: int) -> None:
        """Wraps the buffer the engine fills for this topic.

        Args:
            topic: The topic.
            channel: Where the engine puts the topic's messages.
            engine: The engine, for unsubscribing on `close`.
            generation: Which subscription this is, so closing an old one for
                a topic subscribed again doesn't remove the new one.
        """
        self._topic = topic
        self._channel = channel
        self._engine = engine
        self._generation = generation
        self.replay_from: int | None = None
        self.replay_to: int | None = None

    @property
    def topic(self) -> str:
        """The subscribed topic.

        Returns:
            The topic name.
        """
        return self._topic

    async def next(self) -> Message | None:
        """Waits for the next message.

        Returns:
            The message, or None once the subscription has ended.
        """
        return await self._channel.get()

    def __aiter__(self) -> Subscription:
        """Returns the subscription itself as the iterator.

        Returns:
            The subscription.
        """
        return self

    async def __anext__(self) -> Message:
        """Waits for the next message.

        Returns:
            The message.

        Raises:
            StopAsyncIteration: When the subscription has ended.
        """
        message = await self._channel.get()
        if message is None:
            raise StopAsyncIteration
        return message

    def close(self) -> None:
        """Stops listening and unsubscribes without waiting for the reply.

        Buffered messages are dropped. Does nothing the second time.
        """
        if self._channel.receiver_closed:
            return
        self._channel.close_receiver()
        self._engine.forget(self._topic, self._generation)

    async def __aenter__(self) -> Subscription:
        """Enters an `async with` block.

        Returns:
            The subscription.
        """
        return self

    async def __aexit__(self, *exc_info: object) -> None:
        """Closes the subscription when the `async with` block ends.

        Args:
            *exc_info: The exception that ended the block, if any.
        """
        self.close()


@dataclass(frozen=True)
class Event:
    """Something that happened to the connection or a subscription.

    Attributes:
        name: The event's name, as the scenario driver reports it.
    """

    name: ClassVar[str] = "event"

    def to_json(self) -> dict[str, Any]:
        """Describes the event as a JSON object.

        Returns:
            `{"event": name, ...fields}`.
        """
        encoded: dict[str, Any] = {"event": self.name}
        encoded.update({key: list(value) if isinstance(value, tuple) else value for key, value in vars(self).items()})
        return encoded


@dataclass(frozen=True)
class Connected(Event):
    """A connection is up and authenticated.

    Attributes:
        resumed: Topics the server restored for the durable session.
    """

    name: ClassVar[str] = "connected"
    resumed: tuple[str, ...] = ()


@dataclass(frozen=True)
class Disconnected(Event):
    """The connection went down.

    Attributes:
        code: The server's close code, or None when it sent none PopSub
            defines.
        reconnecting: Whether the client will try to reconnect.
    """

    name: ClassVar[str] = "disconnected"
    code: int | None = None
    reconnecting: bool = True


@dataclass(frozen=True)
class Truncated(Event):
    """Messages on a topic were trimmed before the client got them.

    Attributes:
        topic: The topic.
        first_available: The oldest seq still available; delivery continues
            from here.
    """

    name: ClassVar[str] = "truncated"
    topic: str = ""
    first_available: int = 0


@dataclass(frozen=True)
class Stalled(Event):
    """A QoS 1 message ran out of retries.

    The server paused the topic for this client until it acknowledges
    something or subscribes again.

    Attributes:
        topic: The topic.
        seq: The message that was never acknowledged.
    """

    name: ClassVar[str] = "stalled"
    topic: str = ""
    seq: int = 0


@dataclass(frozen=True)
class AuthExpiring(Event):
    """The token expires soon.

    With password credentials the client logs in again on its own; with a
    bare token the connection closes at expiry.

    Attributes:
        expires_at_ms: When it expires, in milliseconds since the Unix epoch.
    """

    name: ClassVar[str] = "auth_expiring"
    expires_at_ms: int = 0


@dataclass(frozen=True)
class AuthRefreshed(Event):
    """The client got a new token and the connection carries on."""

    name: ClassVar[str] = "auth_refreshed"


@dataclass(frozen=True)
class Unsubscribed(Event):
    """The server ended a subscription because the user may no longer read the topic.

    Its `Subscription` ends and the client won't resubscribe.

    Attributes:
        topic: The topic.
    """

    name: ClassVar[str] = "unsubscribed"
    topic: str = ""


@dataclass(frozen=True)
class ServerErrorEvent(Event):
    """An `error` frame that answers no request of the application.

    Examples are a failed QoS 0 publish or a failed resubscribe after a
    reconnect.

    Attributes:
        code: The error code.
        message: The server's explanation.
    """

    name: ClassVar[str] = "server_error"
    code: str = ""
    message: str = ""


class EventStream:
    """The client's events from the moment it was created, as an async iterator.

    Holds up to `EVENT_BUFFER` events; when a reader falls further behind,
    the oldest are dropped. Iteration ends after the client stops and the
    remaining events are read.
    """

    def __init__(self) -> None:
        """Builds an empty, open stream."""
        self._events: deque[Event] = deque(maxlen=EVENT_BUFFER)
        self._ready = asyncio.Event()
        self._ended = False

    def _push(self, event: Event) -> None:
        """Adds an event.

        Args:
            event: The event.
        """
        if not self._ended:
            self._events.append(event)
            self._ready.set()

    def _end(self) -> None:
        """Marks the stream finished once drained."""
        self._ended = True
        self._ready.set()

    async def next(self) -> Event | None:
        """Waits for the next event.

        Returns:
            The event, or None once the client has stopped and every event
            has been read.
        """
        while not self._events:
            if self._ended:
                return None
            self._ready.clear()
            await self._ready.wait()
        return self._events.popleft()

    def __aiter__(self) -> EventStream:
        """Returns the stream itself as the iterator.

        Returns:
            The stream.
        """
        return self

    async def __anext__(self) -> Event:
        """Waits for the next event.

        Returns:
            The event.

        Raises:
            StopAsyncIteration: Once the client has stopped and the stream
                is drained.
        """
        event = await self.next()
        if event is None:
            raise StopAsyncIteration
        return event


class EventHub:
    """Fans events out to every open `EventStream`."""

    def __init__(self) -> None:
        """Builds a hub with no streams."""
        self._streams: list[EventStream] = []
        self._ended = False

    def stream(self) -> EventStream:
        """Opens a stream of every event from now on.

        Returns:
            The stream; already ended if the client has stopped.
        """
        stream = EventStream()
        if self._ended:
            stream._end()
        else:
            self._streams.append(stream)
        return stream

    def emit(self, event: Event) -> None:
        """Sends an event to every stream.

        Args:
            event: The event.
        """
        for stream in self._streams:
            stream._push(event)

    def end(self) -> None:
        """Ends every stream, after the client stopped."""
        self._ended = True
        for stream in self._streams:
            stream._end()
        self._streams.clear()
