use thiserror::Error;

#[derive(Error, Debug)]
pub enum PopSubError {
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tungstenite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Lock error")]
    Lock,

    #[error("Client error: {0}")]
    Client(String),

    #[error("Persistence error: {0}")]
    Persistence(String),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),
}

pub type Result<T> = std::result::Result<T, PopSubError>;
