"""End-to-end tests of `Client` against the in-process fake server.

They check what the scenario suite can't see from outside: that closing the
client leaves no task running, that nothing logs a destroyed pending task,
and what the `auth` frame carries with and without a session id.
"""

from __future__ import annotations

import asyncio
import gc
import logging
from collections.abc import Iterator

import pytest

from popsub import (
    Client,
    ClientConfig,
    ClosedError,
    ConnectError,
    Disconnected,
    PasswordCredentials,
    ReconnectPolicy,
    ServerError,
    StartFrom,
)

from .fake_server import PASSWORD, FakeServer


@pytest.fixture
def server() -> Iterator[FakeServer]:
    """Runs a fake server for one test.

    Yields:
        The running server.
    """
    fake = FakeServer()
    fake.start()
    yield fake
    fake.stop()


async def acks_seen(server: FakeServer, count: int, within: float = 2.0) -> list[int]:
    """Waits for the fake server to record a number of acks.

    The server runs in another thread, so an ack the client has sent may not
    be recorded yet when the test looks.

    Args:
        server: The server.
        count: How many acks to wait for.
        within: The longest to wait, in seconds.

    Returns:
        The acknowledged seqs in the order the server recorded them, once
        there are `count`, or as they stand when the time runs out.
    """
    loop = asyncio.get_running_loop()
    deadline = loop.time() + within
    while True:
        acks = [frame["seq"] for frame in server.frames("ack")]
        if len(acks) >= count or loop.time() >= deadline:
            return acks
        await asyncio.sleep(0.01)


def make_config(server: FakeServer, password: str = PASSWORD, session_id: str | None = None) -> ClientConfig:
    """Builds a configuration with fast reconnects for the fake server.

    Args:
        server: The server.
        password: The password to log in with.
        session_id: The durable session to hold; None is ephemeral.

    Returns:
        The configuration.
    """
    return ClientConfig(
        server.url,
        PasswordCredentials("alice", password),
        reconnect=ReconnectPolicy(initial=0.02, max=0.2),
        request_timeout=5.0,
        session_id=session_id,
    )


def other_tasks() -> set[asyncio.Task[object]]:
    """Lists the event loop's unfinished tasks other than the current one.

    Returns:
        The tasks.
    """
    return {task for task in asyncio.all_tasks() if task is not asyncio.current_task() and not task.done()}


@pytest.mark.asyncio
async def test_messages_arrive_once_and_duplicates_are_reacked(server: FakeServer) -> None:
    """Each seq reaches the application once; the resent copy is acknowledged again."""
    server.duplicate_deliveries = True
    client = await Client.connect(make_config(server))
    subscription = await client.subscribe("orders", StartFrom.new())
    first = await client.publish_confirmed("orders", "one")
    second = await client.publish_confirmed("orders", "two")
    assert (first.seq, second.seq) == (1, 2)
    received = [await subscription.next(), await subscription.next()]
    assert [message.seq for message in received if message is not None] == [1, 2]
    assert [message.message_id for message in received if message is not None] == [
        first.message_id,
        second.message_id,
    ]
    await client.publish("orders", "three")
    third = await subscription.next()
    assert third is not None
    assert third.seq == 3
    assert await acks_seen(server, 4) == [1, 1, 2, 2]
    await client.close()


@pytest.mark.asyncio
async def test_close_leaves_no_tasks(server: FakeServer) -> None:
    """After `close`, the client has no task left and its subscriptions have ended."""
    client = await Client.connect(make_config(server))
    events = client.events()
    subscription = await client.subscribe("news", StartFrom.new())
    await client.close()
    assert other_tasks() == set()
    assert await subscription.next() is None
    assert await events.next() is None
    with pytest.raises(ClosedError):
        await client.publish("news", "late")


@pytest.mark.asyncio
async def test_close_while_reconnecting_leaves_no_tasks(server: FakeServer) -> None:
    """Closing during the reconnect backoff stops at once and leaves no task."""
    client = await Client.connect(make_config(server))
    events = client.events()
    await asyncio.to_thread(server.stop)
    event = await asyncio.wait_for(events.next(), 5)
    assert isinstance(event, Disconnected)
    assert event.code == 1001
    assert event.reconnecting
    with pytest.raises(ConnectError):
        await client.admin().list_users()
    await asyncio.sleep(0.1)
    await client.close()
    assert other_tasks() == set()
    assert client.stopped


@pytest.mark.asyncio
async def test_reconnect_resubscribes_from_next_expected(server: FakeServer) -> None:
    """After a dropped connection the topic is subscribed again from the next seq it expects."""
    client = await Client.connect(make_config(server))
    events = client.events()
    subscription = await client.subscribe("feed", StartFrom.last(5))
    await client.publish_confirmed("feed", "one")
    message = await subscription.next()
    assert message is not None
    assert message.seq == 1
    await asyncio.to_thread(server.drop_connections, 1001)
    while not isinstance(await asyncio.wait_for(events.next(), 5), Disconnected):
        pass
    await client.publish_confirmed("feed", "two")
    message = await asyncio.wait_for(subscription.next(), 5)
    assert message is not None
    assert message.seq == 2
    froms = [frame["from"] for frame in server.frames("subscribe")]
    assert froms == [{"last": 5}, {"seq": 2}]
    await client.close()


@pytest.mark.asyncio
async def test_auth_asks_for_the_hold_only_with_a_session(server: FakeServer) -> None:
    """The `auth` frame carries `hold_resumed` with a session id and omits it without one."""
    ephemeral = await Client.connect(make_config(server))
    await ephemeral.close()
    durable = await Client.connect(make_config(server, session_id="alice-laptop"))
    await durable.close()
    without, with_session = server.frames("auth")
    assert "hold_resumed" not in without
    assert "session_id" not in without
    assert with_session["session_id"] == "alice-laptop"
    assert with_session["hold_resumed"] is True


@pytest.mark.asyncio
async def test_held_is_read_from_authenticated_and_absent_means_false(server: FakeServer) -> None:
    """The engine records `held` when the server answers with it and false when the reply has none."""
    unheld = await Client.connect(make_config(server, session_id="alice-laptop"))
    assert unheld._engine._held is False
    await unheld.close()
    server.honours_hold = True
    held = await Client.connect(make_config(server, session_id="alice-laptop"))
    assert held._engine._held is True
    await held.close()


@pytest.mark.asyncio
async def test_rejected_login_fails_connect(server: FakeServer) -> None:
    """A rejected login fails `connect` with `invalid_credentials` and leaves no task."""
    with pytest.raises(ServerError) as caught:
        await Client.connect(make_config(server, password="wrong-password"))
    assert caught.value.code == "invalid_credentials"
    assert other_tasks() == set()


def test_no_destroyed_pending_task_warnings(server: FakeServer, caplog: pytest.LogCaptureFixture) -> None:
    """A full client lifecycle in its own event loop logs nothing about destroyed pending tasks."""

    async def lifecycle() -> None:
        """Connects, subscribes, publishes, loses the connection and closes."""
        client = await Client.connect(make_config(server))
        subscription = await client.subscribe("t", StartFrom.new())
        await client.publish_confirmed("t", "x")
        await subscription.next()
        await asyncio.to_thread(server.drop_connections, 1001)
        await asyncio.sleep(0.05)
        await client.close()
        assert other_tasks() == set()

    with caplog.at_level(logging.ERROR, logger="asyncio"):
        asyncio.run(lifecycle())
        gc.collect()
    assert not [record for record in caplog.records if "destroyed but it is pending" in record.getMessage()]
