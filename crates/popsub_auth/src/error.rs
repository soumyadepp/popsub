//! Authentication errors
//!
//! Custom error types for the auth module.

use thiserror::Error;

/// Authentication and authorization errors.
#[derive(Error, Debug)]
pub enum AuthError {
    /// Invalid username or password during login.
    #[error("invalid credentials")]
    InvalidCredentials,

    /// The provided JWT token is invalid or expired.
    #[error("invalid or expired token")]
    InvalidToken,

    /// Token has expired.
    #[error("token has expired")]
    TokenExpired,

    /// User does not exist.
    #[error("user not found: {0}")]
    UserNotFound(String),

    /// User already exists (during registration).
    #[error("user already exists: {0}")]
    UserAlreadyExists(String),

    /// User is not authorized to perform the requested action.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// User is not authorized to access the topic.
    #[error("not authorized to {action} on topic '{topic}'")]
    TopicNotAuthorized { topic: String, action: String },

    /// Password hashing error.
    #[error("password hashing error")]
    PasswordHashError,

    /// JWT encoding/decoding error.
    #[error("JWT error: {0}")]
    JwtError(#[from] jsonwebtoken::errors::Error),

    /// Internal error.
    #[error("internal auth error: {0}")]
    Internal(String),
}

/// Result type alias for auth operations.
pub type Result<T> = std::result::Result<T, AuthError>;
