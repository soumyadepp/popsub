//! Authentication and topic authorization for the PopSub broker.
//!
//! The crate issues and checks JWTs (`JwtService`), stores users behind the
//! `UserStore` trait (in memory by default, PostgreSQL with the `postgres`
//! feature), and decides per topic whether a user may subscribe or publish
//! (`PermissionChecker`). `AuthService` ties the three together and is what the
//! broker calls.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                       AuthService                           │
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
//! │  │ JwtService  │  │  UserStore  │  │  PermissionChecker  │  │
//! │  │             │  │  (trait)    │  │                     │  │
//! │  │ - generate  │  │             │  │  - can_subscribe    │  │
//! │  │ - validate  │  │ - get_user  │  │  - can_publish      │  │
//! │  │ - refresh   │  │ - add_user  │  │                     │  │
//! │  └─────────────┘  └─────────────┘  └─────────────────────┘  │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! Example:
//!
//! ```rust,ignore
//! use popsub_auth::{AuthService, AuthConfig, Role, TopicPermission};
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = AuthConfig::default();
//!     let auth = AuthService::new(config).await;
//!
//!     auth.add_user(
//!         "alice",
//!         "secure_password",
//!         Role::User {
//!             allowed_topics: vec![
//!                 TopicPermission::new("chat/#", true, true),
//!                 TopicPermission::new("sensors/#", true, false),
//!             ],
//!         },
//!     ).await.unwrap();
//!
//!     let token = auth.login("alice", "secure_password").await.unwrap();
//!
//!     let claims = auth.validate_token(&token).unwrap();
//!     assert!(auth.can_publish(&claims.sub, "chat/room1").await);
//!     assert!(!auth.can_publish(&claims.sub, "sensors/temp").await);
//! }
//! ```
//!
//! To store users in PostgreSQL, enable the `postgres` feature:
//!
//! ```toml
//! popsub_auth = { version = "0.1", features = ["postgres"] }
//! ```
//!
//! Example:
//!
//! ```rust,ignore
//! use sqlx::postgres::PgPoolOptions;
//! use popsub_auth::{AuthService, AuthConfig};
//! use popsub_auth::store::PostgresUserStore;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let pool = PgPoolOptions::new()
//!         .max_connections(5)
//!         .connect("postgres://user:pass@localhost/popsub")
//!         .await
//!         .unwrap();
//!
//!     let store = Arc::new(PostgresUserStore::new_with_migrations(pool).await.unwrap());
//!     let config = AuthConfig::default();
//!     let auth = AuthService::with_store(config, store).await;
//! }
//! ```

pub mod error;
pub mod file_store;
pub mod jwt;
pub mod permission;
pub mod service;
pub mod store;
pub mod user;

pub use error::AuthError;
pub use file_store::FileUserStore;
pub use jwt::{Claims, JwtService};
pub use permission::{MAX_PATTERN_BYTES, PermissionChecker, TopicPermission, validate_pattern};
pub use service::{AuthConfig, AuthService, DefaultUserRole};
pub use store::{InMemoryUserStore, UserStore};
pub use user::{Role, User};

#[cfg(test)]
mod tests;
