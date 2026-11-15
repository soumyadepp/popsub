//! HS256 JWT claims and the service that signs, checks and refreshes them.
//!
//! `AuthService` owns one `JwtService` built from `AuthConfig::jwt_secret`. Tokens
//! carry the username and, when issued at login, the user's role name.

use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::error::{AuthError, Result};

/// The payload of a PopSub token: who it was issued to, when, and until when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// The username the token was issued to.
    pub sub: String,
    /// Expiration time as Unix timestamp (seconds since epoch).
    pub exp: usize,
    /// Issue time as a Unix timestamp (seconds since epoch).
    pub iat: usize,
    /// Role name (`admin`, `user` or `readonly`) at issue time, omitted from the
    /// token when `None`. Permission checks read the role from the store, not
    /// from this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Claims {
    /// Builds claims issued now and expiring `expiration_hours` from now, with no role.
    ///
    /// Args:
    ///     username: The subject of the token.
    ///     expiration_hours: Hours until the token expires.
    ///
    /// Returns:
    ///     Claims with `iat` set to now and `role` set to `None`.
    pub fn new(username: impl Into<String>, expiration_hours: u64) -> Self {
        let now = Utc::now();
        let exp = now + Duration::hours(expiration_hours as i64);

        Self {
            sub: username.into(),
            exp: exp.timestamp() as usize,
            iat: now.timestamp() as usize,
            role: None,
        }
    }

    /// Builds claims like `Claims::new` and records a role name in them.
    ///
    /// Args:
    ///     username: The subject of the token.
    ///     expiration_hours: Hours until the token expires.
    ///     role: The role name to embed.
    ///
    /// Returns:
    ///     Claims with `role` set to `Some(role)`.
    pub fn with_role(username: impl Into<String>, expiration_hours: u64, role: &str) -> Self {
        let mut claims = Self::new(username, expiration_hours);
        claims.role = Some(role.to_string());
        claims
    }

    /// Reports whether `exp` is at or before the current time, with no leeway.
    ///
    /// Returns:
    ///     `true` once the expiry time has been reached.
    pub fn is_expired(&self) -> bool {
        let now = Utc::now().timestamp() as usize;
        self.exp <= now
    }

    /// Counts the seconds left until `exp`.
    ///
    /// Returns:
    ///     The seconds remaining, or 0 once the token has expired.
    pub fn remaining_seconds(&self) -> u64 {
        let now = Utc::now().timestamp() as usize;
        if self.exp > now {
            (self.exp - now) as u64
        } else {
            0
        }
    }
}

/// Signs and verifies HS256 tokens with one shared secret and a fixed lifetime.
///
/// Its `Debug` output shows only the lifetime, never the keys.
#[derive(Clone)]
pub struct JwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    validation: Validation,
    expiration_hours: u64,
}

impl JwtService {
    /// Builds a service that signs and verifies with `secret` using HS256.
    ///
    /// Verification uses `jsonwebtoken`'s default `Validation`, which checks
    /// `exp` with a 60 second leeway.
    ///
    /// Args:
    ///     secret: The HMAC secret, used for both signing and verifying.
    ///     expiration_hours: Lifetime of every token this service issues.
    ///
    /// Returns:
    ///     The configured service.
    pub fn new(secret: &str, expiration_hours: u64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            validation: Validation::default(),
            expiration_hours,
        }
    }

    /// Signs a token for `username` with no role claim.
    ///
    /// Args:
    ///     username: The subject of the token.
    ///
    /// Returns:
    ///     The encoded token.
    ///
    /// Raises:
    ///     AuthError::JwtError: When `jsonwebtoken` fails to encode the claims.
    pub fn generate_token(&self, username: &str) -> Result<String> {
        let claims = Claims::new(username, self.expiration_hours);
        let token = encode(&Header::default(), &claims, &self.encoding_key)?;
        Ok(token)
    }

    /// Signs a token for `username` that also carries a role name.
    ///
    /// Args:
    ///     username: The subject of the token.
    ///     role: The role name to embed.
    ///
    /// Returns:
    ///     The encoded token.
    ///
    /// Raises:
    ///     AuthError::JwtError: When `jsonwebtoken` fails to encode the claims.
    pub fn generate_token_with_role(&self, username: &str, role: &str) -> Result<String> {
        let claims = Claims::with_role(username, self.expiration_hours, role);
        let token = encode(&Header::default(), &claims, &self.encoding_key)?;
        Ok(token)
    }

    /// Verifies a token's signature and expiry and decodes its claims.
    ///
    /// Args:
    ///     token: The encoded token.
    ///
    /// Returns:
    ///     The decoded claims.
    ///
    /// Raises:
    ///     AuthError::JwtError: When the token is malformed, its signature does
    ///         not match the secret, or its `exp` is more than the 60 second
    ///         decode leeway in the past.
    ///     AuthError::TokenExpired: When `exp` has passed but is still within
    ///         that leeway.
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        let token_data = decode::<Claims>(token, &self.decoding_key, &self.validation)?;

        if token_data.claims.is_expired() {
            return Err(AuthError::TokenExpired);
        }

        Ok(token_data.claims)
    }

    /// Issues a fresh token for the subject of a still valid token.
    ///
    /// The new token keeps the old token's role claim, if any, and gets a full
    /// lifetime from now. The user store is not consulted, so a removed or
    /// disabled user holding a valid token can keep refreshing it indefinitely.
    ///
    /// Args:
    ///     token: The token to refresh.
    ///
    /// Returns:
    ///     The new encoded token.
    ///
    /// Raises:
    ///     AuthError::JwtError: When the token is malformed, its signature does
    ///         not match the secret, its `exp` is more than the 60 second decode
    ///         leeway in the past, or encoding the new token fails.
    ///     AuthError::TokenExpired: When `exp` has passed but is still within
    ///         that leeway.
    pub fn refresh_token(&self, token: &str) -> Result<String> {
        let claims = self.validate_token(token)?;
        match claims.role.as_deref() {
            Some(role) => self.generate_token_with_role(&claims.sub, role),
            None => self.generate_token(&claims.sub),
        }
    }

    /// Exposes the signing key so other code can sign tokens this service accepts.
    ///
    /// Returns:
    ///     The HS256 encoding key.
    pub fn encoding_key(&self) -> &EncodingKey {
        &self.encoding_key
    }

    /// Exposes the verification key so other code can decode PopSub tokens.
    ///
    /// Returns:
    ///     The HS256 decoding key.
    pub fn decoding_key(&self) -> &DecodingKey {
        &self.decoding_key
    }

    /// Exposes the validation rules used when decoding tokens.
    ///
    /// Returns:
    ///     The `jsonwebtoken` validation settings.
    pub fn validation(&self) -> &Validation {
        &self.validation
    }
}

impl std::fmt::Debug for JwtService {
    /// Writes the service with its lifetime and leaves out the keys, so secrets never reach logs.
    ///
    /// Args:
    ///     f: The formatter to write to.
    ///
    /// Returns:
    ///     The formatter's result.
    ///
    /// Raises:
    ///     std::fmt::Error: When the formatter fails to write.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtService")
            .field("expiration_hours", &self.expiration_hours)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod jwt_tests {
    use super::*;

    /// Checks that a freshly generated token validates and carries the username.
    #[test]
    fn test_generate_and_validate_token() {
        let service = JwtService::new("test_secret", 24);

        let token = service.generate_token("alice").unwrap();
        let claims = service.validate_token(&token).unwrap();

        assert_eq!(claims.sub, "alice");
        assert!(!claims.is_expired());
    }

    /// Checks that a role passed at generation comes back in the validated claims.
    #[test]
    fn test_token_with_role() {
        let service = JwtService::new("test_secret", 24);

        let token = service.generate_token_with_role("bob", "admin").unwrap();
        let claims = service.validate_token(&token).unwrap();

        assert_eq!(claims.sub, "bob");
        assert_eq!(claims.role, Some("admin".to_string()));
    }

    /// Checks that a malformed token string fails validation.
    #[test]
    fn test_invalid_token() {
        let service = JwtService::new("test_secret", 24);

        let result = service.validate_token("invalid.token.here");
        assert!(result.is_err());
    }

    /// Checks that refreshing a valid token yields a token for the same user.
    #[test]
    fn test_refresh_token() {
        let service = JwtService::new("test_secret", 24);

        let token = service.generate_token("charlie").unwrap();
        let new_token = service.refresh_token(&token).unwrap();

        let claims = service.validate_token(&new_token).unwrap();
        assert_eq!(claims.sub, "charlie");
    }
}
