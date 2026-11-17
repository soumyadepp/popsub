"""Bookkeeping with no I/O.

Per-topic deduplication and acknowledgement memory, and the bounded buffer
between the connection and the application.

Kept apart from the connection code so it can be tested on its own.
"""

from __future__ import annotations

import asyncio
from collections import deque
from dataclasses import dataclass, field
from typing import Generic, TypeVar

from popsub.config import AckMode
from popsub.protocol import StartFrom

ACK_MEMORY = 4096
"""How many acknowledged seqs a topic remembers, to re-acknowledge duplicates."""

T = TypeVar("T")


def is_pattern(topic: str) -> bool:
    """Says whether a subscribe target is a wildcard pattern.

    Args:
        topic: The topic or pattern.

    Returns:
        True when it contains `+` or `#`.
    """
    return "+" in topic or "#" in topic


def matches_pattern(pattern: str, topic: str) -> bool:
    """Says whether a topic matches a wildcard pattern.

    Levels are separated by `/`. A `+` level matches any one level, and a
    trailing `#` matches the rest, including nothing (`orders/#` matches
    `orders`). Every other level must be equal.

    Args:
        pattern: The pattern, such as `orders/#` or `sensors/+/temp`.
        topic: The topic name.

    Returns:
        True when the topic matches.
    """
    wanted = pattern.split("/")
    levels = topic.split("/")
    for index, level in enumerate(wanted):
        if level == "#":
            return index == len(wanted) - 1
        if index >= len(levels):
            return False
        if level not in ("+", levels[index]):
            return False
    return len(wanted) == len(levels)


class AckMemory:
    """The seqs a subscription acknowledged recently, oldest forgotten first."""

    def __init__(self, limit: int = ACK_MEMORY) -> None:
        """Builds an empty memory.

        Args:
            limit: How many seqs to remember.
        """
        self._limit = limit
        self._seen: set[int] = set()
        self._order: deque[int] = deque()

    def insert(self, seq: int) -> None:
        """Remembers an acknowledged seq, forgetting the oldest beyond the limit.

        Args:
            seq: The acknowledged message.
        """
        if seq in self._seen:
            return
        self._seen.add(seq)
        self._order.append(seq)
        if len(self._order) > self._limit:
            self._seen.discard(self._order.popleft())

    def __contains__(self, seq: object) -> bool:
        """Says whether a seq is remembered as acknowledged.

        Args:
            seq: The message.

        Returns:
            True if it is remembered.
        """
        return seq in self._seen

    def __len__(self) -> int:
        """Counts the remembered seqs.

        Returns:
            How many are remembered.
        """
        return len(self._order)


@dataclass(frozen=True)
class Verdict:
    """What to do with one delivered message.

    Attributes:
        deliver: Hand it to the application.
        ack: Send an `ack` for it now.
    """

    deliver: bool
    ack: bool


@dataclass
class TopicState:
    """What the client knows about one subscribed topic.

    A shared subscription (one read through a group) gets only some of the
    topic's messages, out of order, so it finds duplicates with the bounded
    `seen` set and never uses `next_expected`.

    Attributes:
        start: Where replay started when the application subscribed.
        next_expected: The next seq the application should see; anything
            lower is a duplicate. None until the server says where the
            subscription starts. Unused when `shared`.
        acked: What the application (or automatic mode) acknowledged.
        shared: Whether the topic is read through a group.
        seen: The seqs handed to the application, for a shared subscription.
    """

    start: StartFrom
    next_expected: int | None = None
    acked: AckMemory = field(default_factory=AckMemory)
    shared: bool = False
    seen: AckMemory = field(default_factory=AckMemory)

    def advance(self, seq: int) -> None:
        """Moves the next expected seq forward, never back.

        Used for `subscribed.replay_from` and `truncated.first_available`.
        Does nothing for a shared subscription, whose position is the
        group's and lives on the server.

        Args:
            seq: The seq delivery continues from.
        """
        if self.shared:
            return
        if self.next_expected is None or seq > self.next_expected:
            self.next_expected = seq

    def is_duplicate(self, seq: int) -> bool:
        """Says whether the application already got a seq.

        Args:
            seq: The message's seq.

        Returns:
            For a shared subscription, whether the seq is in `seen`;
            otherwise whether it is below the next expected seq.
        """
        if self.shared:
            return seq in self.seen
        return self.next_expected is not None and seq < self.next_expected

    def accept(self, seq: int, qos: int, ack_mode: AckMode) -> Verdict:
        """Decides what to do with a delivered message, updating the state.

        A message below the next expected seq (or, for a shared
        subscription, one already in `seen`) is a duplicate: it never
        reaches the application, but a QoS 1 duplicate is acknowledged again
        when it was acknowledged before or in automatic mode, so a lost ack
        can't stall the subscription. A new QoS 1 message in automatic mode
        is acknowledged as it is handed over.

        Args:
            seq: The message's seq.
            qos: Its QoS, 0 or 1.
            ack_mode: The client's acknowledgement mode.

        Returns:
            Whether to deliver it and whether to acknowledge it now.
        """
        if self.is_duplicate(seq):
            again = qos == 1 and (ack_mode == AckMode.AUTO or seq in self.acked)
            return Verdict(deliver=False, ack=again)
        if self.shared:
            self.seen.insert(seq)
        else:
            self.next_expected = seq + 1
        if qos == 1 and ack_mode == AckMode.AUTO:
            self.acked.insert(seq)
            return Verdict(deliver=True, ack=True)
        return Verdict(deliver=True, ack=False)

    def resubscribe_from(self) -> StartFrom:
        """Where to subscribe again after a reconnect.

        Returns:
            `"resume"` for a shared subscription. Otherwise
            `{"seq": next_expected}` once the server has said where the
            subscription starts, or the original start before that.
        """
        if self.shared:
            return StartFrom.resume()
        if self.next_expected is None:
            return self.start
        return StartFrom.seq(self.next_expected)


async def _first(*waits: asyncio.Event) -> None:
    """Waits until any of several events is set.

    Args:
        *waits: The events.
    """
    tasks = [asyncio.ensure_future(event.wait()) for event in waits]
    try:
        await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
    finally:
        for task in tasks:
            task.cancel()
        await asyncio.wait(tasks)


class Channel(Generic[T]):
    """A bounded single-consumer queue with separate ends for closing.

    The sender closes it when the subscription ends: the receiver gets what
    is buffered, then None. The receiver closes it when the application stops
    listening: buffered items are dropped and further puts fail.
    """

    def __init__(self, capacity: int) -> None:
        """Builds an empty channel.

        Args:
            capacity: How many items it holds before `put` waits.
        """
        self._capacity = max(1, capacity)
        self._items: deque[T] = deque()
        self._sender_closed = False
        self._receiver_closed = False
        self._readable = asyncio.Event()
        self._writable = asyncio.Event()
        self._writable.set()

    @property
    def receiver_closed(self) -> bool:
        """Says whether the receiving side stopped listening.

        Returns:
            True after `close_receiver`.
        """
        return self._receiver_closed

    def __len__(self) -> int:
        """Counts the buffered items.

        Returns:
            How many are waiting.
        """
        return len(self._items)

    async def put(self, item: T, abort: asyncio.Event | None = None) -> bool:
        """Adds an item, waiting while the channel is full.

        Args:
            item: The item.
            abort: Stops waiting when set; the item is then dropped.

        Returns:
            True once added; False when either end closed or `abort` was set
            while waiting.
        """
        while True:
            if self._sender_closed or self._receiver_closed:
                return False
            if len(self._items) < self._capacity:
                self._items.append(item)
                self._readable.set()
                return True
            if abort is not None and abort.is_set():
                return False
            self._writable.clear()
            if abort is None:
                await self._writable.wait()
            else:
                await _first(self._writable, abort)

    async def get(self) -> T | None:
        """Takes the oldest item, waiting while the channel is empty.

        Returns:
            The item, or None once the channel is closed and drained.
        """
        while not self._items:
            if self._sender_closed or self._receiver_closed:
                return None
            self._readable.clear()
            await self._readable.wait()
        item = self._items.popleft()
        self._writable.set()
        return item

    def close_sender(self) -> None:
        """Ends the channel from the sending side; buffered items stay readable."""
        self._sender_closed = True
        self._readable.set()
        self._writable.set()

    def close_receiver(self) -> None:
        """Ends the channel from the receiving side, dropping buffered items."""
        self._receiver_closed = True
        self._items.clear()
        self._readable.set()
        self._writable.set()
