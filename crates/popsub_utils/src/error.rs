//! The workspace error type, `PopSubError`, and the `Result` alias built on it.

use thiserror::Error;

/// Errors returned across PopSub crates.
///
/// Variants marked `#[from]` convert automatically with `?`. `Config` wraps
/// `config` 0.13 errors, while `popsub_config` uses `config` 0.15, so errors
/// from the settings loader don't convert into it.
#[derive(Error, Debug)]
pub enum PopSubError {
    /// A value couldn't be encoded or decoded as JSON.
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A JWT couldn't be created or failed validation.
    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    /// A WebSocket handshake, read or write failed.
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tungstenite::Error),

    /// An operating system I/O call failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A frame couldn't be handed to a connected client, described by the message.
    #[error("Client error: {0}")]
    Client(String),

    /// Settings couldn't be loaded or had the wrong type.
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),
}

/// `std::result::Result` with `PopSubError` as the error type.
pub type Result<T> = std::result::Result<T, PopSubError>;
