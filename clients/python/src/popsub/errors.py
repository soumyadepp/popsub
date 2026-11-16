"""The exceptions client calls raise.

Every one is a `PopSubError` with a `kind` naming the failure: `server`,
`closed`, `timeout`, `connect`, `protocol` or `config`.
"""

from __future__ import annotations

from typing import ClassVar

from popsub.protocol import is_temporary_code


class PopSubError(Exception):
    """Base class of every client error.

    Attributes:
        kind: What failed: `server`, `closed`, `timeout`, `connect`,
            `protocol` or `config`.
        message: What went wrong, for people.
    """

    kind: ClassVar[str] = "protocol"

    def __init__(self, message: str) -> None:
        """Builds the error.

        Args:
            message: What went wrong, for people.
        """
        super().__init__(message)
        self.message = message

    @property
    def is_temporary(self) -> bool:
        """Says whether retrying the same call later might work.

        Returns:
            False here; subclasses for temporary failures override it.
        """
        return False


class ServerError(PopSubError):
    """The server refused the request with an `error` frame.

    Attributes:
        code: The error code, such as `not_authorized`. Unknown codes are
            kept as sent and treated like `internal`.
        retry_after: Seconds to wait before trying again, for limits.
    """

    kind: ClassVar[str] = "server"

    def __init__(self, code: str, message: str, retry_after: int | None = None) -> None:
        """Builds the error.

        Args:
            code: The error code.
            message: The server's explanation.
            retry_after: Seconds to wait, for `locked_out` and `rate_limited`.
        """
        super().__init__(message)
        self.code = code
        self.retry_after = retry_after

    def __str__(self) -> str:
        """Describes the refusal.

        Returns:
            The code and the server's explanation.
        """
        return f"the server refused the request ({self.code}): {self.message}"

    @property
    def is_temporary(self) -> bool:
        """Says whether the refusal is temporary.

        Returns:
            True for `rate_limited`, `locked_out`, `storage_unavailable`,
            `session_unavailable`, `internal` and unknown codes.
        """
        return is_temporary_code(self.code)


class ClosedError(PopSubError):
    """The client has stopped for good.

    It was closed by the application, the server closed it with a code that
    rules out reconnecting (such as `session_taken_over`), or it ran out of
    reconnect attempts.

    Attributes:
        close_code: The close code that stopped it, or None.
    """

    kind: ClassVar[str] = "closed"

    def __init__(self, close_code: int | None) -> None:
        """Builds the error.

        Args:
            close_code: The close code that stopped the client, or None.
        """
        super().__init__(f"the client is closed (close code {close_code})")
        self.close_code = close_code


class RequestTimeoutError(PopSubError):
    """The request didn't complete within `ClientConfig.request_timeout`."""

    kind: ClassVar[str] = "timeout"

    def __init__(self, message: str = "the request timed out") -> None:
        """Builds the error.

        Args:
            message: What timed out.
        """
        super().__init__(message)

    @property
    def is_temporary(self) -> bool:
        """Says that a timeout is temporary.

        Returns:
            True.
        """
        return True


class ConnectError(PopSubError):
    """The server couldn't be reached, or the connection dropped before an admin reply."""

    kind: ClassVar[str] = "connect"

    @property
    def is_temporary(self) -> bool:
        """Says that a connection failure is temporary.

        Returns:
            True.
        """
        return True


class ProtocolError(PopSubError):
    """The server sent something this client doesn't expect."""

    kind: ClassVar[str] = "protocol"


class ConfigError(PopSubError):
    """The configuration is invalid."""

    kind: ClassVar[str] = "config"
