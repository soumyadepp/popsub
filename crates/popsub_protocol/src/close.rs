//! Why the server closes a connection, sent as the WebSocket close code.
//!
//! The close code tells a client SDK what to do next without parsing text:
//! reconnect with backoff, log in again first, or stop. When the server
//! closes after a failed request it also sends an `error` frame first, with
//! the matching `ErrorCode`.

/// A close code the server sends, with what a client should do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseCode {
    /// 1001: the server is shutting down. Reconnect with backoff.
    GoingAway,
    /// 1009: the client sent a frame larger than the server accepts. Reconnect,
    /// but don't resend that frame.
    MessageTooBig,
    /// 1011: the server hit an unexpected error. Reconnect with backoff.
    InternalError,
    /// 4001: the token or `session_id` in `auth` was rejected. Log in again
    /// (or fix the session id) before reconnecting.
    AuthFailed,
    /// 4002: a request came before `auth`. A client bug; reconnect and
    /// authenticate first.
    Unauthenticated,
    /// 4003: another connection authenticated with the same session. Don't
    /// reconnect automatically, or the two will keep taking it from each other.
    SessionTakenOver,
    /// 4004: the token expired and wasn't refreshed with a new `auth`. Log in
    /// again and reconnect.
    TokenExpired,
    /// 4005: nothing arrived from the client, not even a pong, for the idle
    /// timeout. Reconnect.
    IdleTimeout,
    /// 4006: the server doesn't speak the requested protocol version. Don't
    /// reconnect with the same version.
    UnsupportedProtocol,
    /// 4007: the session store couldn't be reached during `auth`. Reconnect
    /// with backoff.
    SessionUnavailable,
}

impl CloseCode {
    /// Every close code, for docs and tests.
    pub const ALL: [CloseCode; 10] = [
        CloseCode::GoingAway,
        CloseCode::MessageTooBig,
        CloseCode::InternalError,
        CloseCode::AuthFailed,
        CloseCode::Unauthenticated,
        CloseCode::SessionTakenOver,
        CloseCode::TokenExpired,
        CloseCode::IdleTimeout,
        CloseCode::UnsupportedProtocol,
        CloseCode::SessionUnavailable,
    ];

    /// The numeric WebSocket close code.
    ///
    /// Returns:
    ///     1001, 1009 or 1011 for the standard codes, 4001 to 4007 for PopSub's own.
    pub fn code(self) -> u16 {
        match self {
            CloseCode::GoingAway => 1001,
            CloseCode::MessageTooBig => 1009,
            CloseCode::InternalError => 1011,
            CloseCode::AuthFailed => 4001,
            CloseCode::Unauthenticated => 4002,
            CloseCode::SessionTakenOver => 4003,
            CloseCode::TokenExpired => 4004,
            CloseCode::IdleTimeout => 4005,
            CloseCode::UnsupportedProtocol => 4006,
            CloseCode::SessionUnavailable => 4007,
        }
    }

    /// The short reason sent with the close code.
    ///
    /// Returns:
    ///     A snake_case reason, such as `session_taken_over`.
    pub fn reason(self) -> &'static str {
        match self {
            CloseCode::GoingAway => "going_away",
            CloseCode::MessageTooBig => "message_too_big",
            CloseCode::InternalError => "internal_error",
            CloseCode::AuthFailed => "auth_failed",
            CloseCode::Unauthenticated => "unauthenticated",
            CloseCode::SessionTakenOver => "session_taken_over",
            CloseCode::TokenExpired => "token_expired",
            CloseCode::IdleTimeout => "idle_timeout",
            CloseCode::UnsupportedProtocol => "unsupported_protocol",
            CloseCode::SessionUnavailable => "session_unavailable",
        }
    }

    /// Recognises a close code the server sent.
    ///
    /// Args:
    ///     code: The numeric close code.
    ///
    /// Returns:
    ///     The close code, or `None` for one the server doesn't use.
    pub fn from_code(code: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.code() == code)
    }

    /// Whether a client should reconnect on its own after this close.
    ///
    /// Returns:
    ///     False when reconnecting would fail the same way or fight another
    ///     connection: a taken-over session, an unsupported protocol, or
    ///     rejected credentials (which need a new login first).
    pub fn should_reconnect(self) -> bool {
        !matches!(
            self,
            CloseCode::SessionTakenOver | CloseCode::UnsupportedProtocol | CloseCode::AuthFailed
        )
    }
}
