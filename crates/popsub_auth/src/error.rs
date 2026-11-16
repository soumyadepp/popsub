//! Error type and `Result` alias shared by every part of the auth crate.

use thiserror::Error;

/// Every failure the auth crate reports, from bad credentials to store errors.
#[derive(Error, Debug)]
pub enum AuthError {
    /// Login failed because the user is unknown, disabled, or gave the wrong password.
    #[error("invalid credentials")]
    InvalidCredentials,

    /// The token verifies but can't be honoured, because its user has been
    /// removed or disabled. Returned by `AuthService::refresh_token`.
    #[error("invalid or expired token")]
    InvalidToken,

    /// The token decoded but its `exp` time has passed.
    #[error("token has expired")]
    TokenExpired,

    /// No user with the given username exists in the store.
    #[error("user not found: {0}")]
    UserNotFound(String),

    /// A user with the given username is already in the store.
    #[error("user already exists: {0}")]
    UserAlreadyExists(String),

    /// The action is not allowed. Also used when registration is disabled or
    /// a username or password breaks the registration rules.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// The user may not perform `action` on `topic`.
    #[error("not authorized to {action} on topic '{topic}'")]
    TopicNotAuthorized {
        /// The topic the user tried to use.
        topic: String,
        /// The refused action, `"subscribe"` or `"publish"`.
        action: String,
    },

    /// Argon2 could not hash a password or parse a stored hash.
    #[error("password hashing error")]
    PasswordHashError,

    /// The `jsonwebtoken` crate failed to encode or decode a token, including
    /// a bad signature or an `exp` already past at decode time.
    #[error("JWT error: {0}")]
    JwtError(#[from] jsonwebtoken::errors::Error),

    /// A store failed for a reason outside the auth rules, such as a poisoned
    /// lock or a database error.
    #[error("internal auth error: {0}")]
    Internal(String),
}

impl AuthError {
    /// The kind of error, without the names or text it carries, for logs
    /// that must not record who it was about.
    ///
    /// Returns:
    ///     A snake_case name, such as `user_already_exists`.
    pub fn kind(&self) -> &'static str {
        match self {
            AuthError::InvalidCredentials => "invalid_credentials",
            AuthError::InvalidToken => "invalid_token",
            AuthError::TokenExpired => "token_expired",
            AuthError::UserNotFound(_) => "user_not_found",
            AuthError::UserAlreadyExists(_) => "user_already_exists",
            AuthError::PermissionDenied(_) => "permission_denied",
            AuthError::TopicNotAuthorized { .. } => "topic_not_authorized",
            AuthError::PasswordHashError => "password_hash_error",
            AuthError::JwtError(_) => "jwt_error",
            AuthError::Internal(_) => "internal",
        }
    }
}

/// `Result` with `AuthError` as the error type, used by every fallible auth call.
pub type Result<T> = std::result::Result<T, AuthError>;
