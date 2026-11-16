//! The error every storage backend returns, and the `Result` alias built on it.
//!
//! The variants tell the server how to react, not what went wrong inside the
//! backend: retry on `Unavailable`, stop serving on `Corrupt`, refuse the
//! configuration on `Unsupported`. The backend's own message is kept as text.

use thiserror::Error;

/// Errors returned by `MessageLog`, `SubscriptionStore` and `Storage` methods.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The backend couldn't be reached or refused the operation for now, so a
    /// retry may succeed. Disk full and lost database connections land here.
    #[error("storage unavailable: {0}")]
    Unavailable(String),

    /// Stored data failed a checksum or couldn't be decoded.
    #[error("corrupt stored data: {0}")]
    Corrupt(String),

    /// The backend can't do what was asked, such as `Durability::Synced` on a
    /// backend that never writes to disk.
    #[error("unsupported by this backend: {0}")]
    Unsupported(String),

    /// Any other backend failure.
    #[error("storage error: {0}")]
    Other(String),
}

/// `std::result::Result` with `StorageError` as the error type.
pub type Result<T> = std::result::Result<T, StorageError>;
