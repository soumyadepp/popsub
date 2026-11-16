//! The errors the client returns.

use popsub_protocol::{CloseCode, ErrorCode};
use thiserror::Error;

/// Why a client call failed.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Error {
    /// The server refused the request.
    #[error("the server refused the request ({code:?}): {message}")]
    Server {
        /// What went wrong, for programs.
        code: ErrorCode,
        /// What went wrong, for people.
        message: String,
        /// Seconds to wait before trying again, for limits.
        retry_after: Option<u64>,
    },
    /// The client has stopped for good: it was closed, the server closed it
    /// in a way that rules out reconnecting (such as `SessionTakenOver`), or
    /// it ran out of reconnect attempts.
    #[error("the client is closed ({0:?})")]
    Closed(Option<CloseCode>),
    /// The request didn't complete within `ClientConfig::request_timeout`.
    #[error("the request timed out")]
    Timeout,
    /// The server couldn't be reached or the WebSocket handshake failed.
    #[error("can't connect: {0}")]
    Connect(String),
    /// The server sent something this client doesn't expect.
    #[error("unexpected reply from the server: {0}")]
    Protocol(String),
}

impl Error {
    /// Whether retrying the same thing later might work.
    ///
    /// Returns:
    ///     True for timeouts, connection failures and the server's
    ///     temporary refusals (`rate_limited`, `locked_out`,
    ///     `storage_unavailable`, `session_unavailable`, `internal`).
    pub fn is_temporary(&self) -> bool {
        match self {
            Error::Timeout | Error::Connect(_) => true,
            Error::Server { code, .. } => matches!(
                code,
                ErrorCode::RateLimited
                    | ErrorCode::LockedOut
                    | ErrorCode::StorageUnavailable
                    | ErrorCode::SessionUnavailable
                    | ErrorCode::Internal
            ),
            Error::Closed(_) | Error::Protocol(_) => false,
        }
    }
}
