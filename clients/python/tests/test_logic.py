"""Unit tests for the client's pure logic: backoff, deduplication, and wire types."""

from __future__ import annotations

import pytest

from popsub import (
    AckMode,
    ClientConfig,
    CloseCode,
    ConfigError,
    PasswordCredentials,
    ReconnectPolicy,
    RoleSpec,
    ServerError,
    StartFrom,
    UserInfo,
)
from popsub._state import AckMemory, TopicState


def test_backoff_ceiling_doubles_from_initial() -> None:
    """The ceiling starts at `initial` and doubles with each failed attempt."""
    policy = ReconnectPolicy(initial=0.25, max=30.0)
    assert [policy.ceiling(attempt) for attempt in range(4)] == [0.25, 0.5, 1.0, 2.0]


def test_backoff_ceiling_is_capped_at_max() -> None:
    """The ceiling never passes `max`, even after very many attempts."""
    policy = ReconnectPolicy(initial=0.02, max=0.2)
    assert policy.ceiling(4) == 0.2
    assert policy.ceiling(10_000) == 0.2


def test_backoff_ceiling_treats_negative_attempts_as_zero() -> None:
    """A negative attempt count gives the initial ceiling."""
    assert ReconnectPolicy(initial=1.0, max=8.0).ceiling(-3) == 1.0


def test_config_rejects_bad_values() -> None:
    """Validation refuses a non-WebSocket URL, a long session id and a zero buffer."""
    credentials = PasswordCredentials("alice", "secret-password")
    with pytest.raises(ConfigError):
        ClientConfig("http://localhost", credentials).validate()
    with pytest.raises(ConfigError):
        ClientConfig("ws://localhost", credentials, session_id="x" * 129).validate()
    with pytest.raises(ConfigError):
        ClientConfig("ws://localhost", credentials, session_id="bad\nid").validate()
    with pytest.raises(ConfigError):
        ClientConfig("ws://localhost", credentials, subscription_buffer=0).validate()
    ClientConfig("ws://localhost", credentials, session_id="phone").validate()


def test_new_messages_are_delivered_and_auto_acked() -> None:
    """In automatic mode a new QoS 1 message is delivered and acknowledged at once."""
    state = TopicState(StartFrom.new())
    state.advance(1)
    verdict = state.accept(1, 1, AckMode.AUTO)
    assert verdict.deliver
    assert verdict.ack
    assert state.next_expected == 2


def test_duplicates_are_dropped_and_reacked_in_auto_mode() -> None:
    """A QoS 1 duplicate never reaches the application but is acknowledged again in automatic mode."""
    state = TopicState(StartFrom.new())
    state.accept(5, 1, AckMode.AUTO)
    verdict = state.accept(5, 1, AckMode.AUTO)
    assert not verdict.deliver
    assert verdict.ack


def test_manual_duplicates_are_reacked_only_once_acked() -> None:
    """In manual mode a duplicate is acknowledged again only if the application acknowledged it."""
    state = TopicState(StartFrom.new())
    first = state.accept(1, 1, AckMode.MANUAL)
    assert first.deliver
    assert not first.ack
    assert state.accept(1, 1, AckMode.MANUAL) == type(first)(deliver=False, ack=False)
    state.acked.insert(1)
    assert state.accept(1, 1, AckMode.MANUAL) == type(first)(deliver=False, ack=True)


def test_qos0_duplicates_are_never_acked() -> None:
    """QoS 0 messages get no acknowledgement, duplicate or not."""
    state = TopicState(StartFrom.new())
    assert not state.accept(1, 0, AckMode.AUTO).ack
    verdict = state.accept(1, 0, AckMode.AUTO)
    assert not verdict.deliver
    assert not verdict.ack


def test_advance_never_moves_back() -> None:
    """`replay_from` and `first_available` only move the next expected seq forward."""
    state = TopicState(StartFrom.new())
    state.advance(10)
    state.advance(4)
    assert state.next_expected == 10
    assert not state.accept(9, 0, AckMode.AUTO).deliver
    assert state.accept(10, 0, AckMode.AUTO).deliver


def test_resubscribe_starts_from_next_expected() -> None:
    """After a reconnect a topic resubscribes from the next seq it expects, not its original start."""
    state = TopicState(StartFrom.last(10))
    assert state.resubscribe_from() == StartFrom.last(10)
    state.advance(3)
    state.accept(3, 1, AckMode.AUTO)
    assert state.resubscribe_from().to_json() == {"seq": 4}


def test_ack_memory_forgets_the_oldest() -> None:
    """The acknowledgement memory keeps only the most recent seqs."""
    memory = AckMemory(limit=3)
    for seq in range(1, 6):
        memory.insert(seq)
    assert 1 not in memory
    assert 2 not in memory
    assert all(seq in memory for seq in (3, 4, 5))
    assert len(memory) == 3


def test_start_from_round_trips() -> None:
    """Every start position encodes and decodes as on the wire."""
    for wire in ("resume", "new", {"last": 3}, {"since_ms": 1_794_800_000_000}, {"seq": 42}):
        assert StartFrom.from_json(wire).to_json() == wire
    for bad in ("later", {"seq": "4"}, {"seq": 1, "last": 2}, None, {"seq": True}):
        with pytest.raises(ValueError, match="start position"):
            StartFrom.from_json(bad)


def test_close_codes_decide_reconnection() -> None:
    """Final close codes rule out reconnecting; unknown codes map to None."""
    final = {code for code in CloseCode if not code.should_reconnect}
    assert final == {
        CloseCode.AUTH_FAILED,
        CloseCode.SESSION_TAKEN_OVER,
        CloseCode.UNSUPPORTED_PROTOCOL,
        CloseCode.ACCOUNT_DISABLED,
    }
    assert CloseCode.from_code(4003) is CloseCode.SESSION_TAKEN_OVER
    assert CloseCode.from_code(1000) is None
    assert CloseCode.from_code(None) is None


def test_user_info_round_trips() -> None:
    """An account decodes and encodes back to the same JSON."""
    wire = {
        "username": "erin",
        "role": {"kind": "user", "topics": [{"pattern": "news/#", "access": "both"}]},
        "enabled": True,
        "created_at_ms": 1_794_800_000_000,
    }
    assert UserInfo.from_json(wire).to_json() == wire
    assert RoleSpec.from_json({"kind": "admin"}).to_json() == {"kind": "admin"}


def test_server_error_temporariness() -> None:
    """Limits and unknown codes are temporary; refusals such as not_authorized are not."""
    assert ServerError("rate_limited", "slow down", 3).is_temporary
    assert ServerError("brand_new_code", "?").is_temporary
    assert not ServerError("invalid_credentials", "no").is_temporary
