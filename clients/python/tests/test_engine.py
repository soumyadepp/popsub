"""Tests of the engine's subscription bookkeeping, with no socket.

They feed the engine commands and server frames directly and record what it
writes, to check the two things the fake server can't show: what a new
process does with a durable session's topics before and while the
application claims them, what it does instead when the server holds those
topics (`authenticated.held`), and what subscribing again to a held wildcard
pattern does to the topics it owned.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any, cast

import pytest

from popsub import ClientConfig, PasswordCredentials, StartFrom
from popsub._engine import Engine, _Connection, _Subscribe
from popsub._state import Channel
from popsub.message import EventHub, Message, Subscription


class Wire:
    """Stands in for the connection and records every frame the engine writes.

    Attributes:
        sent: The frames written, decoded, in order.
    """

    def __init__(self) -> None:
        """Builds a wire with nothing written."""
        self.sent: list[dict[str, Any]] = []

    async def send(self, data: str) -> None:
        """Records one frame.

        Args:
            data: The frame as JSON text.
        """
        self.sent.append(json.loads(data))

    def connection(self) -> _Connection:
        """Presents the wire as the connection type the engine takes.

        Returns:
            The wire, typed as a connection; the engine only calls `send`.
        """
        return cast(_Connection, self)

    def of_type(self, kind: str) -> list[dict[str, Any]]:
        """Lists the written frames of one type, without their ids.

        Args:
            kind: The frame type, such as `subscribe`.

        Returns:
            The frames, in order.
        """
        return [
            {key: value for key, value in frame.items() if key != "id"}
            for frame in self.sent
            if frame.get("type") == kind
        ]


def make_engine() -> Engine:
    """Builds an engine that never connects; it needs a running event loop.

    Returns:
        The engine, in automatic acknowledgement mode.
    """
    config = ClientConfig("ws://127.0.0.1:1", PasswordCredentials("alice", "right-password"))
    return Engine(config, EventHub())


async def subscribe(engine: Engine, wire: Wire, topic: str, start: StartFrom, generation: int) -> Subscription:
    """Subscribes the way `Client.subscribe` does, without waiting for `subscribed`.

    Args:
        engine: The engine.
        wire: Where the engine writes.
        topic: The topic or pattern.
        start: Where replay starts.
        generation: Identifies the subscription.

    Returns:
        The application's subscription.
    """
    channel: Channel[Message] = Channel(16)
    reply: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
    await engine._send_subscribe(wire.connection(), _Subscribe(topic, start, generation, channel, reply))
    return Subscription(topic, channel, engine, generation)


async def receive(engine: Engine, wire: Wire, frame: dict[str, Any]) -> None:
    """Hands the engine one frame as if the server had sent it.

    Args:
        engine: The engine.
        wire: Where the engine writes.
        frame: The frame.
    """
    await engine._on_frame(wire.connection(), frame)


def delivery(topic: str, seq: int) -> dict[str, Any]:
    """Builds a QoS 1 `message` frame.

    Args:
        topic: The topic.
        seq: The seq.

    Returns:
        The frame.
    """
    return {
        "type": "message",
        "topic": topic,
        "seq": seq,
        "message_id": f"{topic}-{seq}",
        "payload": "x",
        "qos": 1,
        "published_at_ms": 1_794_800_000_000,
    }


def buffered(subscription: Subscription) -> list[tuple[str, int]]:
    """Takes what a subscription has buffered, without waiting for more.

    Args:
        subscription: The subscription.

    Returns:
        The topic and seq of each buffered message, in order.
    """
    taken: list[tuple[str, int]] = []
    while len(subscription._channel):
        message = subscription._channel._items.popleft()
        taken.append((message.topic, message.seq))
    return taken


@pytest.mark.asyncio
async def test_unclaimed_topic_messages_are_dropped_without_an_ack() -> None:
    """A resumed topic nobody subscribed to is left unacknowledged, unlike any other untracked topic."""
    engine, wire = make_engine(), Wire()
    engine._note_resumed(["jobs"])
    await receive(engine, wire, delivery("jobs", 1))
    assert wire.sent == []
    await receive(engine, wire, delivery("stray", 1))
    assert wire.of_type("ack") == [{"type": "ack", "topic": "stray", "seq": 1}]


@pytest.mark.asyncio
async def test_tracked_topics_are_not_unclaimed_after_a_reconnect() -> None:
    """A resumed topic the engine already tracks stays tracked and keeps delivering."""
    engine, wire = make_engine(), Wire()
    jobs = await subscribe(engine, wire, "jobs", StartFrom.new(), 1)
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "jobs", "replay_from": 1})
    engine._note_resumed(["jobs"])
    await receive(engine, wire, delivery("jobs", 1))
    assert buffered(jobs) == [("jobs", 1)]
    assert wire.of_type("ack") == [{"type": "ack", "topic": "jobs", "seq": 1}]


@pytest.mark.asyncio
async def test_claiming_by_name_drops_messages_until_subscribed() -> None:
    """A claimed topic drops the server's own resume unacknowledged, then delivers the replay from `replay_from`."""
    engine, wire = make_engine(), Wire()
    engine._note_resumed(["jobs"])
    jobs = await subscribe(engine, wire, "jobs", StartFrom.resume(), 1)
    assert wire.of_type("subscribe") == [{"type": "subscribe", "topic": "jobs", "from": "resume"}]
    await receive(engine, wire, delivery("jobs", 2))
    await receive(engine, wire, delivery("jobs", 3))
    assert buffered(jobs) == []
    assert wire.of_type("ack") == []
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "jobs", "replay_from": 2})
    assert engine._topics["jobs"].state.next_expected == 2
    await receive(engine, wire, delivery("jobs", 2))
    await receive(engine, wire, delivery("jobs", 3))
    assert buffered(jobs) == [("jobs", 2), ("jobs", 3)]
    assert [frame["seq"] for frame in wire.of_type("ack")] == [2, 3]


@pytest.mark.asyncio
async def test_pattern_claims_matching_topics_by_name_from_resume() -> None:
    """A pattern subscribes by name from `resume` to each unclaimed topic it matches and leaves the rest unclaimed."""
    engine, wire = make_engine(), Wire()
    engine._note_resumed(["tasks/b", "jobs", "tasks/a"])
    tasks = await subscribe(engine, wire, "tasks/#", StartFrom.new(), 1)
    assert wire.of_type("subscribe") == [
        {"type": "subscribe", "topic": "tasks/#", "from": "new"},
        {"type": "subscribe", "topic": "tasks/a", "from": "resume"},
        {"type": "subscribe", "topic": "tasks/b", "from": "resume"},
    ]
    claim = next(frame for frame in wire.sent if frame.get("topic") == "tasks/a")
    await receive(engine, wire, delivery("tasks/a", 1))
    await receive(engine, wire, delivery("jobs", 1))
    assert buffered(tasks) == []
    assert wire.of_type("ack") == []
    await receive(engine, wire, {"type": "subscribed", "id": claim["id"], "topic": "tasks/a", "replay_from": 1})
    await receive(engine, wire, delivery("tasks/a", 1))
    await receive(engine, wire, delivery("tasks/b", 1))
    assert buffered(tasks) == [("tasks/a", 1)]
    assert wire.of_type("ack") == [{"type": "ack", "topic": "tasks/a", "seq": 1}]
    assert engine._unclaimed == {"jobs"}


@pytest.mark.asyncio
async def test_reconnect_resubscribes_topics_still_being_claimed() -> None:
    """After a reconnect an unanswered claim is sent again even though the server lists the topic as resumed."""
    engine, wire = make_engine(), Wire()
    engine._note_resumed(["jobs", "tasks/a", "settled"])
    await subscribe(engine, wire, "jobs", StartFrom.last(3), 1)
    await subscribe(engine, wire, "tasks/#", StartFrom.new(), 2)
    await subscribe(engine, wire, "settled", StartFrom.resume(), 3)
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "settled", "replay_from": 4})
    engine._fail_requests()
    again = Wire()
    resumed = ["jobs", "tasks/a", "settled"]
    engine._note_resumed(resumed)
    await engine._restore(again.connection(), resumed)
    assert again.of_type("subscribe") == [
        {"type": "subscribe", "topic": "jobs", "from": {"last": 3}},
        {"type": "subscribe", "topic": "tasks/a", "from": "resume"},
        {"type": "subscribe", "topic": "tasks/#", "from": {"seq": 1}},
    ]
    assert engine._unclaimed == set()
    await receive(engine, again, delivery("jobs", 1))
    assert again.of_type("ack") == []


@pytest.mark.asyncio
async def test_held_session_has_no_unclaimed_topics() -> None:
    """With `held` no resumed topic is unclaimed, so a subscribe is not a claim and its messages are delivered."""
    engine, wire = make_engine(), Wire()
    engine._held = True
    engine._note_resumed(["jobs", "tasks/a"])
    assert engine._unclaimed == set()
    jobs = await subscribe(engine, wire, "jobs", StartFrom.resume(), 1)
    assert wire.of_type("subscribe") == [{"type": "subscribe", "topic": "jobs", "from": "resume"}]
    assert engine._topics["jobs"].claiming is False
    await receive(engine, wire, delivery("jobs", 2))
    assert buffered(jobs) == [("jobs", 2)]
    assert wire.of_type("ack") == [{"type": "ack", "topic": "jobs", "seq": 2}]


@pytest.mark.asyncio
async def test_held_pattern_subscribe_sends_no_claims_by_name() -> None:
    """With `held` a pattern subscribe sends only the pattern, leaving the resumed topics it matches to the server."""
    engine, wire = make_engine(), Wire()
    engine._held = True
    engine._note_resumed(["tasks/a", "tasks/b", "jobs"])
    await subscribe(engine, wire, "tasks/#", StartFrom.new(), 1)
    assert wire.of_type("subscribe") == [{"type": "subscribe", "topic": "tasks/#", "from": "new"}]
    assert set(engine._topics) == set()


@pytest.mark.asyncio
async def test_held_reconnect_resubscribes_resumed_topics_from_resume() -> None:
    """With `held` a reconnect subscribes again from `resume` to each tracked topic the server lists as resumed."""
    engine, wire = make_engine(), Wire()
    engine._held = True
    await subscribe(engine, wire, "jobs", StartFrom.last(3), 1)
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "jobs", "replay_from": 1})
    await receive(engine, wire, delivery("jobs", 1))
    await subscribe(engine, wire, "fresh", StartFrom.new(), 2)
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "fresh", "replay_from": 5})
    channel: Channel[Message] = Channel(16)
    reply: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
    command = _Subscribe("work", StartFrom.resume(), 3, channel, reply, "pool")
    await engine._send_subscribe(wire.connection(), command)
    await subscribe(engine, wire, "tasks/#", StartFrom.new(), 4)
    await receive(engine, wire, {"type": "subscribed", "topic": "tasks/a", "replay_from": 1})
    engine._fail_requests()
    again = Wire()
    resumed = ["jobs", "tasks/a", "elsewhere"]
    engine._note_resumed(resumed)
    await engine._restore(again.connection(), resumed)
    assert again.of_type("subscribe") == [
        {"type": "subscribe", "topic": "jobs", "from": "resume"},
        {"type": "subscribe", "topic": "fresh", "from": {"seq": 5}},
        {"type": "subscribe", "topic": "work", "from": "resume", "group": "pool"},
        {"type": "subscribe", "topic": "tasks/a", "from": "resume"},
        {"type": "subscribe", "topic": "tasks/#", "from": {"seq": 1}},
    ]
    assert engine._unclaimed == set()


@pytest.mark.asyncio
async def test_unheld_reconnect_leaves_resumed_topics_to_the_server() -> None:
    """Without `held` a reconnect sends no subscribe for a tracked topic the server lists as resumed."""
    engine, wire = make_engine(), Wire()
    await subscribe(engine, wire, "jobs", StartFrom.new(), 1)
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "jobs", "replay_from": 1})
    engine._fail_requests()
    again = Wire()
    engine._note_resumed(["jobs"])
    await engine._restore(again.connection(), ["jobs"])
    assert again.of_type("subscribe") == []


@pytest.mark.asyncio
async def test_repeat_pattern_subscribe_moves_owned_topics_to_the_new_subscription() -> None:
    """Subscribing again to a held pattern keeps each owned topic's next seq and delivers only to the new one."""
    engine, wire = make_engine(), Wire()
    first = await subscribe(engine, wire, "orders/#", StartFrom.new(), 1)
    await receive(engine, wire, {"type": "subscribed", "id": wire.sent[-1]["id"], "topic": "orders/#"})
    await receive(engine, wire, {"type": "subscribed", "topic": "orders/eu", "replay_from": 1})
    await receive(engine, wire, delivery("orders/eu", 1))
    await receive(engine, wire, delivery("orders/eu", 2))
    assert buffered(first) == [("orders/eu", 1), ("orders/eu", 2)]
    second = await subscribe(engine, wire, "orders/#", StartFrom.new(), 2)
    assert len(wire.of_type("subscribe")) == 2
    assert await first.next() is None
    assert engine._topics["orders/eu"].state.next_expected == 3
    await receive(engine, wire, delivery("orders/eu", 2))
    await receive(engine, wire, delivery("orders/eu", 3))
    assert buffered(second) == [("orders/eu", 3)]
    assert [frame["seq"] for frame in wire.of_type("ack")] == [1, 2, 2, 3]


@pytest.mark.asyncio
async def test_closing_a_replaced_pattern_subscription_loses_nothing() -> None:
    """Closing the earlier subscription of a replaced pattern unsubscribes nothing and joined topics keep arriving."""
    engine, wire = make_engine(), Wire()
    first = await subscribe(engine, wire, "orders/#", StartFrom.new(), 1)
    await receive(engine, wire, {"type": "subscribed", "topic": "orders/eu", "replay_from": 1})
    await receive(engine, wire, delivery("orders/eu", 1))
    second = await subscribe(engine, wire, "orders/#", StartFrom.new(), 2)
    first.close()
    assert not await engine._run_commands(wire.connection())
    assert wire.of_type("unsubscribe") == []
    assert set(engine._topics) == {"orders/eu"}
    await receive(engine, wire, delivery("orders/eu", 2))
    await receive(engine, wire, {"type": "subscribed", "topic": "orders/us", "replay_from": 1})
    await receive(engine, wire, delivery("orders/us", 1))
    assert buffered(second) == [("orders/eu", 2), ("orders/us", 1)]
    second.close()
    assert not await engine._run_commands(wire.connection())
    assert [frame["topic"] for frame in wire.of_type("unsubscribe")] == ["orders/#", "orders/eu", "orders/us"]
