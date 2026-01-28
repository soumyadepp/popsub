//! JWT token service
//!
//! Handles JWT token generation, validation, and refresh operations.

use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::error::{AuthError, Result};

/// JWT claims structure.
///
/// Contains the subject (username) and expiration time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject - the username this token was issued to.
    pub sub: String,
    /// Expiration time as Unix timestamp (seconds since epoch).
    pub exp: usize,
    /// Issued at time as Unix timestamp.
    pub iat: usize,
    /// Optional role information embedded in the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Claims {
    /// Create new claims for a user.
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

    /// Create new claims with a role.
    pub fn with_role(username: impl Into<String>, expiration_hours: u64, role: &str) -> Self {
        let mut claims = Self::new(username, expiration_hours);
        claims.role = Some(role.to_string());
        claims
    }

    /// Check if the token has expired.
    pub fn is_expired(&self) -> bool {
        let now = Utc::now().timestamp() as usize;
        self.exp <= now
    }

    /// Get remaining validity in seconds (0 if expired).
    pub fn remaining_seconds(&self) -> u64 {
        let now = Utc::now().timestamp() as usize;
        if self.exp > now {
            (self.exp - now) as u64
        } else {
            0
        }
    }
}

/// JWT service for token generation and validation.
#[derive(Clone)]
pub struct JwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    validation: Validation,
    expiration_hours: u64,
}

impl JwtService {
    /// Create a new JWT service with the given secret.
    pub fn new(secret: &str, expiration_hours: u64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            validation: Validation::default(),
            expiration_hours,
        }
    }

    /// Generate a new token for a user.
    pub fn generate_token(&self, username: &str) -> Result<String> {
        let claims = Claims::new(username, self.expiration_hours);
        let token = encode(&Header::default(), &claims, &self.encoding_key)?;
        Ok(token)
    }

    /// Generate a token with role information.
    pub fn generate_token_with_role(&self, username: &str, role: &str) -> Result<String> {
        let claims = Claims::with_role(username, self.expiration_hours, role);
        let token = encode(&Header::default(), &claims, &self.encoding_key)?;
        Ok(token)
    }

    /// Validate a token and return its claims.
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        let token_data = decode::<Claims>(token, &self.decoding_key, &self.validation)?;

        if token_data.claims.is_expired() {
            return Err(AuthError::TokenExpired);
        }

        Ok(token_data.claims)
    }

    /// Refresh a token (issue a new token for the same user).
    ///
    /// The old token must still be valid (not expired).
    pub fn refresh_token(&self, token: &str) -> Result<String> {
        let claims = self.validate_token(token)?;
        self.generate_token(&claims.sub)
    }

    /// Get the encoding key (for external use if needed).
    pub fn encoding_key(&self) -> &EncodingKey {
        &self.encoding_key
    }

    /// Get the decoding key (for external use if needed).
    pub fn decoding_key(&self) -> &DecodingKey {
        &self.decoding_key
    }

    /// Get the validation settings (for external use if needed).
    pub fn validation(&self) -> &Validation {
        &self.validation
    }
}

impl std::fmt::Debug for JwtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtService")
            .field("expiration_hours", &self.expiration_hours)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod jwt_tests {
    use super::*;

    #[test]
    fn test_generate_and_validate_token() {
        let service = JwtService::new("test_secret", 24);

        let token = service.generate_token("alice").unwrap();
        let claims = service.validate_token(&token).unwrap();

        assert_eq!(claims.sub, "alice");
        assert!(!claims.is_expired());
    }

    #[test]
    fn test_token_with_role() {
        let service = JwtService::new("test_secret", 24);

        let token = service.generate_token_with_role("bob", "admin").unwrap();
        let claims = service.validate_token(&token).unwrap();

        assert_eq!(claims.sub, "bob");
        assert_eq!(claims.role, Some("admin".to_string()));
    }

    #[test]
    fn test_invalid_token() {
        let service = JwtService::new("test_secret", 24);

        let result = service.validate_token("invalid.token.here");
        assert!(result.is_err());
    }

    #[test]
    fn test_refresh_token() {
        let service = JwtService::new("test_secret", 24);

        let token = service.generate_token("charlie").unwrap();
        let new_token = service.refresh_token(&token).unwrap();

        let claims = service.validate_token(&new_token).unwrap();
        assert_eq!(claims.sub, "charlie");
    }
}
