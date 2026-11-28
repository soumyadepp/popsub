"""Wire-level types of PopSub protocol version 1.

These mirror `popsub_protocol` in the Rust workspace and the JSON Schemas in
`docs/protocol/`: where a subscription starts, how a payload encodes bytes,
roles and grants for the admin requests, account records, close codes and
error codes.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import Any

PROTOCOL_VERSION = 1
"""The protocol version this client names in `auth`."""

ERROR_CODES = frozenset(
    {
        "bad_request",
        "unauthenticated",
        "auth_failed",
        "invalid_credentials",
        "locked_out",
        "rate_limited",
        "not_authorized",
        "registration_failed",
        "invalid_session_id",
        "session_unavailable",
        "unsupported_protocol",
        "storage_unavailable",
        "payload_too_large",
        "invalid_topic",
        "internal",
        "forbidden",
        "user_not_found",
        "user_exists",
        "topic_not_found",
        "backup_exists",
        "backup_unsupported",
        "topic_limit_reached",
    }
)
"""Every error code protocol version 1 defines. Newer servers may add more."""

TEMPORARY_ERROR_CODES = frozenset(
    {"rate_limited", "locked_out", "storage_unavailable", "session_unavailable", "internal"}
)
"""Error codes for refusals that may succeed if tried again later."""


def is_temporary_code(code: str) -> bool:
    """Says whether an error code describes a temporary refusal.

    An unknown code counts as `internal`, as the protocol asks.

    Args:
        code: The error code from an `error` frame.

    Returns:
        True when retrying later might succeed.
    """
    if code not in ERROR_CODES:
        return True
    return code in TEMPORARY_ERROR_CODES


class CloseCode(enum.IntEnum):
    """The WebSocket close codes a PopSub server sends."""

    GOING_AWAY = 1001
    MESSAGE_TOO_BIG = 1009
    INTERNAL_ERROR = 1011
    TRY_AGAIN_LATER = 1013
    AUTH_FAILED = 4001
    UNAUTHENTICATED = 4002
    SESSION_TAKEN_OVER = 4003
    TOKEN_EXPIRED = 4004
    IDLE_TIMEOUT = 4005
    UNSUPPORTED_PROTOCOL = 4006
    SESSION_UNAVAILABLE = 4007
    SLOW_CONSUMER = 4008
    ACCOUNT_DISABLED = 4009

    @property
    def should_reconnect(self) -> bool:
        """Says whether the server allows reconnecting after this code.

        `TOKEN_EXPIRED` counts as yes here; the client additionally needs
        password credentials to log in again.

        Returns:
            False for `AUTH_FAILED`, `SESSION_TAKEN_OVER`,
            `UNSUPPORTED_PROTOCOL` and `ACCOUNT_DISABLED`, True otherwise.
        """
        return self not in _FINAL_CLOSE_CODES

    @classmethod
    def from_code(cls, code: int | None) -> CloseCode | None:
        """Looks up a close code.

        Args:
            code: The numeric code from a close frame, or None.

        Returns:
            The matching member, or None for codes PopSub doesn't define
            (such as 1000 or 1006) and for None.
        """
        if code is None:
            return None
        try:
            return cls(code)
        except ValueError:
            return None


_FINAL_CLOSE_CODES = frozenset(
    {
        CloseCode.AUTH_FAILED,
        CloseCode.SESSION_TAKEN_OVER,
        CloseCode.UNSUPPORTED_PROTOCOL,
        CloseCode.ACCOUNT_DISABLED,
    }
)


@dataclass(frozen=True)
class StartFrom:
    """Where replay starts when subscribing.

    Build one with the class methods: `resume()`, `new()`, `last(n)`,
    `since_ms(t)` or `seq(s)`.

    Attributes:
        kind: One of `resume`, `new`, `last`, `since_ms` or `seq`.
        value: The number for `last`, `since_ms` and `seq`; None otherwise.
    """

    kind: str
    value: int | None = None

    @classmethod
    def resume(cls) -> StartFrom:
        """One past a durable session's stored cursor; no replay otherwise.

        Returns:
            The start position.
        """
        return cls("resume")

    @classmethod
    def new(cls) -> StartFrom:
        """No replay: only messages published after subscribing.

        Returns:
            The start position.
        """
        return cls("new")

    @classmethod
    def last(cls, count: int) -> StartFrom:
        """The last `count` retained messages.

        Args:
            count: How many.

        Returns:
            The start position.
        """
        return cls("last", count)

    @classmethod
    def since_ms(cls, timestamp_ms: int) -> StartFrom:
        """The first message published at or after a time.

        Args:
            timestamp_ms: Milliseconds since the Unix epoch.

        Returns:
            The start position.
        """
        return cls("since_ms", timestamp_ms)

    @classmethod
    def seq(cls, seq: int) -> StartFrom:
        """This `seq`, or the oldest retained one if it was trimmed.

        Args:
            seq: The position in the topic.

        Returns:
            The start position.
        """
        return cls("seq", seq)

    def to_json(self) -> Any:
        """Encodes the position as on the wire.

        Returns:
            `"resume"`, `"new"`, or a one-key object such as `{"last": 10}`.
        """
        if self.kind in ("resume", "new"):
            return self.kind
        return {self.kind: self.value}

    @classmethod
    def from_json(cls, value: Any) -> StartFrom:
        """Decodes a position from its wire form.

        Args:
            value: `"resume"`, `"new"`, or a one-key object such as
                `{"seq": 42}`.

        Returns:
            The start position.

        Raises:
            ValueError: When the value isn't a valid position.
        """
        if value in ("resume", "new"):
            return cls(value)
        if isinstance(value, dict) and len(value) == 1:
            ((kind, number),) = value.items()
            if kind in ("last", "since_ms", "seq") and isinstance(number, int) and not isinstance(number, bool):
                return cls(kind, number)
        raise ValueError(f"not a start position: {value!r}")


class Encoding(str, enum.Enum):
    """How a payload string encodes a message's bytes, when it isn't text."""

    BASE64 = "base64"

    @classmethod
    def from_json(cls, value: Any) -> Encoding | None:
        """Decodes the `encoding` field of a `message` frame.

        Args:
            value: The field's value, or None when the frame has none.

        Returns:
            The encoding, or None for a text payload. A value protocol
            version 1 doesn't define also gives None, so the payload is read
            as text.
        """
        try:
            return cls(value)
        except ValueError:
            return None


class Access(str, enum.Enum):
    """What a topic grant allows."""

    SUBSCRIBE = "subscribe"
    PUBLISH = "publish"
    BOTH = "both"


class RoleKind(str, enum.Enum):
    """The kinds of account."""

    ADMIN = "admin"
    USER = "user"
    READONLY = "readonly"


@dataclass(frozen=True)
class TopicGrant:
    """One topic pattern an account may use.

    Attributes:
        pattern: The topic pattern, with `+` and `#` wildcards.
        access: What it allows.
    """

    pattern: str
    access: Access

    def to_json(self) -> dict[str, Any]:
        """Encodes the grant as on the wire.

        Returns:
            `{"pattern": ..., "access": ...}`.
        """
        return {"pattern": self.pattern, "access": self.access.value}

    @classmethod
    def from_json(cls, value: Any) -> TopicGrant:
        """Decodes a grant.

        Args:
            value: `{"pattern": ..., "access": ...}`.

        Returns:
            The grant.

        Raises:
            ValueError: When the value isn't a valid grant.
        """
        if not isinstance(value, dict) or not isinstance(value.get("pattern"), str):
            raise ValueError(f"not a topic grant: {value!r}")
        return cls(value["pattern"], Access(value.get("access")))


@dataclass(frozen=True)
class RoleSpec:
    """What an account may do.

    Attributes:
        kind: The kind of account.
        topics: Its grants, or None to leave them out on the wire. Ignored
            for admins.
    """

    kind: RoleKind
    topics: tuple[TopicGrant, ...] | None = None

    def to_json(self) -> dict[str, Any]:
        """Encodes the role as on the wire.

        Returns:
            `{"kind": ..., "topics": [...]}`, without `topics` when None.
        """
        encoded: dict[str, Any] = {"kind": self.kind.value}
        if self.topics is not None:
            encoded["topics"] = [grant.to_json() for grant in self.topics]
        return encoded

    @classmethod
    def from_json(cls, value: Any) -> RoleSpec:
        """Decodes a role.

        Args:
            value: `{"kind": ..., "topics": [...]}`; `topics` may be absent.

        Returns:
            The role.

        Raises:
            ValueError: When the value isn't a valid role.
        """
        if not isinstance(value, dict):
            raise ValueError(f"not a role: {value!r}")
        kind = RoleKind(value.get("kind"))
        topics = value.get("topics")
        if topics is None:
            return cls(kind)
        if not isinstance(topics, list):
            raise ValueError(f"role topics must be a list: {topics!r}")
        return cls(kind, tuple(TopicGrant.from_json(grant) for grant in topics))


@dataclass(frozen=True)
class UserInfo:
    """An account, as the admin requests return it. Never carries a password.

    Attributes:
        username: Its name.
        role: What it may do.
        enabled: Whether it may log in.
        created_at_ms: When it was created, in milliseconds since the Unix epoch.
    """

    username: str
    role: RoleSpec
    enabled: bool
    created_at_ms: int

    def to_json(self) -> dict[str, Any]:
        """Encodes the account as on the wire.

        Returns:
            The `user` object of a `user` frame.
        """
        return {
            "username": self.username,
            "role": self.role.to_json(),
            "enabled": self.enabled,
            "created_at_ms": self.created_at_ms,
        }

    @classmethod
    def from_json(cls, value: Any) -> UserInfo:
        """Decodes an account.

        Args:
            value: The `user` object of a `user` frame.

        Returns:
            The account.

        Raises:
            ValueError: When the value isn't a valid account.
        """
        if not isinstance(value, dict):
            raise ValueError(f"not a user: {value!r}")
        try:
            return cls(
                username=str(value["username"]),
                role=RoleSpec.from_json(value["role"]),
                enabled=bool(value["enabled"]),
                created_at_ms=int(value["created_at_ms"]),
            )
        except (KeyError, TypeError) as error:
            raise ValueError(f"not a user: {value!r}") from error


@dataclass(frozen=True)
class TopicInfo:
    """A topic's log, as the topic admin requests return it.

    Attributes:
        name: The topic.
        first_seq: The oldest seq still stored.
        last_seq: The newest seq ever stored; the next publish gets one more.
        messages: How many messages are stored.
        subscribers: How many subscriptions the topic has now.
    """

    name: str
    first_seq: int
    last_seq: int
    messages: int
    subscribers: int

    def to_json(self) -> dict[str, Any]:
        """Encodes the topic as on the wire.

        Returns:
            The `topic` object of a `topic_info` frame.
        """
        return {
            "name": self.name,
            "first_seq": self.first_seq,
            "last_seq": self.last_seq,
            "messages": self.messages,
            "subscribers": self.subscribers,
        }

    @classmethod
    def from_json(cls, value: Any) -> TopicInfo:
        """Decodes a topic.

        Args:
            value: The `topic` object of a `topic_info` frame.

        Returns:
            The topic.

        Raises:
            ValueError: When the value isn't a valid topic object.
        """
        if not isinstance(value, dict):
            raise ValueError(f"not a topic: {value!r}")
        try:
            return cls(
                name=str(value["name"]),
                first_seq=int(value["first_seq"]),
                last_seq=int(value["last_seq"]),
                messages=int(value["messages"]),
                subscribers=int(value["subscribers"]),
            )
        except (KeyError, TypeError) as error:
            raise ValueError(f"not a topic: {value!r}") from error


@dataclass(frozen=True)
class BackupInfo:
    """A backup the server wrote of itself, as the `backup` admin request returns it.

    Attributes:
        name: The backup's directory name.
        path: Where the backup is on the server's file system.
        topics: How many topics it holds.
        records: How many messages it holds across every topic.
        bytes: How many bytes were written.
        users: Whether accounts were included; false when the server doesn't
            keep them in `users.json`.
    """

    name: str
    path: str
    topics: int
    records: int
    bytes: int
    users: bool

    def to_json(self) -> dict[str, Any]:
        """Encodes the backup as on the wire.

        Returns:
            The `backup` object of a `backup_created` frame.
        """
        return {
            "name": self.name,
            "path": self.path,
            "topics": self.topics,
            "records": self.records,
            "bytes": self.bytes,
            "users": self.users,
        }

    @classmethod
    def from_json(cls, value: Any) -> BackupInfo:
        """Decodes a backup.

        Args:
            value: The `backup` object of a `backup_created` frame.

        Returns:
            The backup.

        Raises:
            ValueError: When the value isn't a valid backup object.
        """
        if not isinstance(value, dict):
            raise ValueError(f"not a backup: {value!r}")
        try:
            return cls(
                name=str(value["name"]),
                path=str(value["path"]),
                topics=int(value["topics"]),
                records=int(value["records"]),
                bytes=int(value["bytes"]),
                users=bool(value["users"]),
            )
        except (KeyError, TypeError) as error:
            raise ValueError(f"not a backup: {value!r}") from error
