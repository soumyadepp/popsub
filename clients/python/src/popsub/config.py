"""How a `Client` connects: where, as whom, and what it does when things go wrong."""

from __future__ import annotations

import enum
import ssl
from dataclasses import dataclass, field
from typing import TypeAlias

from popsub.errors import ConfigError

MAX_DOUBLINGS = 30
"""How many times the reconnect ceiling doubles at most before `max` caps it anyway."""

MAX_SESSION_ID_CHARS = 128
"""The longest session id the server accepts, in characters."""

_CONTROL_CHARACTERS = frozenset(chr(code) for code in (*range(0x20), 0x7F))
"""Characters a session id must not contain."""


@dataclass(frozen=True)
class PasswordCredentials:
    """A username and password.

    The client logs in on every connection and refreshes its token on its
    own before it expires.

    Attributes:
        username: The username.
        password: The password.
    """

    username: str
    password: str = field(repr=False)


@dataclass(frozen=True)
class TokenCredentials:
    """A JWT obtained elsewhere.

    The client can't refresh it: when it expires the server closes the
    connection with `token_expired` and the client stops, after emitting
    `AuthExpiring`.

    Attributes:
        token: The JWT.
    """

    token: str = field(repr=False)


Credentials: TypeAlias = PasswordCredentials | TokenCredentials
"""Either kind of credentials."""


class AckMode(str, enum.Enum):
    """When QoS 1 messages are acknowledged.

    Attributes:
        AUTO: As the client hands each message to the application. A message
            the application was processing when it crashed isn't redelivered.
        MANUAL: When the application calls `Message.ack`. A message never
            acknowledged is redelivered after a reconnect (to a durable
            session), and stalls its subscription once the server runs out
            of retries.
    """

    AUTO = "auto"
    MANUAL = "manual"


@dataclass(frozen=True)
class ReconnectPolicy:
    """How long the client waits between connection attempts.

    The ceiling doubles from `initial` up to `max`, and each wait is a random
    time between zero and the ceiling, so many clients reconnecting after a
    server restart don't all arrive at once.

    Attributes:
        initial: The ceiling before the first retry, in seconds.
        max: The largest ceiling, in seconds.
        max_attempts: Give up after this many failed attempts in a row;
            None keeps trying.
    """

    initial: float = 0.25
    max: float = 30.0
    max_attempts: int | None = None

    def ceiling(self, attempt: int) -> float:
        """The longest wait before an attempt, before jitter.

        Args:
            attempt: How many attempts in a row have failed, from 0.

        Returns:
            `initial` doubled `attempt` times, capped at `max`, in seconds.
        """
        doublings: int = min(max(attempt, 0), MAX_DOUBLINGS)
        return float(min(self.initial * float(2**doublings), self.max))


@dataclass
class ClientConfig:
    """Everything a `Client` needs to connect.

    Attributes:
        url: The server's WebSocket URL, such as `ws://127.0.0.1:8080`, or
            `wss://` for TLS.
        credentials: Who to connect as.
        session_id: A durable session to hold, so subscriptions and
            unacknowledged messages survive disconnects. None is an
            ephemeral connection.
        clean_start: Discard the session's stored subscriptions on the first
            connection. Never sent on reconnects.
        ack_mode: When QoS 1 messages are acknowledged.
        reconnect: How to reconnect.
        request_timeout: How long a request (connect, subscribe, confirmed
            publish, admin call) may take, reconnects included, in seconds.
        subscription_buffer: How many messages each subscription buffers for
            the application. While one is full the client stops reading the
            socket, and the server paces delivery.
        ssl_context: The TLS settings for `wss://`, such as one trusting a
            private CA. None uses Python's defaults.
    """

    url: str
    credentials: Credentials
    session_id: str | None = None
    clean_start: bool = False
    ack_mode: AckMode = AckMode.AUTO
    reconnect: ReconnectPolicy = field(default_factory=ReconnectPolicy)
    request_timeout: float = 30.0
    subscription_buffer: int = 1024
    ssl_context: ssl.SSLContext | None = None

    def validate(self) -> None:
        """Checks the configuration before connecting.

        Raises:
            ConfigError: When the URL isn't `ws://` or `wss://`, a session id
                isn't 1 to 128 characters without control characters, or a
                duration, count or buffer size is out of range.
        """
        if not (self.url.startswith("ws://") or self.url.startswith("wss://")):
            raise ConfigError(f"the URL must start with ws:// or wss://: {self.url!r}")
        if not isinstance(self.credentials, (PasswordCredentials, TokenCredentials)):
            raise ConfigError("credentials must be PasswordCredentials or TokenCredentials")
        if self.session_id is not None:
            if not 1 <= len(self.session_id) <= MAX_SESSION_ID_CHARS:
                raise ConfigError(f"session_id must be 1 to {MAX_SESSION_ID_CHARS} characters")
            if any(character in _CONTROL_CHARACTERS for character in self.session_id):
                raise ConfigError("session_id must not contain control characters")
        if self.request_timeout <= 0:
            raise ConfigError("request_timeout must be positive")
        if self.subscription_buffer < 1:
            raise ConfigError("subscription_buffer must be at least 1")
        policy = self.reconnect
        if policy.initial < 0 or policy.max < policy.initial:
            raise ConfigError("reconnect needs 0 <= initial <= max")
        if policy.max_attempts is not None and policy.max_attempts < 0:
            raise ConfigError("reconnect.max_attempts must be None or at least 0")
