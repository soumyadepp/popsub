//! popsub_auth
//!
//! Authentication and authorization module for PopSub. This crate provides:
//!
//! - **JWT token generation and validation** via the `JwtService`
//! - **User management** with the `User` struct and `Role`-based permissions
//! - **Role-Based Access Control (RBAC)** for topic-level authorization
//! - **Pluggable user storage** via the `UserStore` trait (InMemory or PostgreSQL)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                       AuthService                           │
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
//! │  │ JwtService  │  │  UserStore  │  │  PermissionChecker  │  │
//! │  │             │  │  (trait)    │  │                     │  │
//! │  │ - generate  │  │             │  │  - can_subscribe    │  │
//! │  │ - validate  │  │ - get_user  │  │  - can_publish      │  │
//! │  │ - refresh   │  │ - add_user  │  │  - can_admin        │  │
//! │  └─────────────┘  └─────────────┘  └─────────────────────┘  │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use popsub_auth::{AuthService, AuthConfig, Role, TopicPermission};
//!
//! #[tokio::main]
//! async fn main() {
//!     // Create auth service with default in-memory store
//!     let config = AuthConfig::default();
//!     let auth = AuthService::new(config).await;
//!
//!     // Add a user with publish/subscribe permissions
//!     auth.add_user(
//!         "alice",
//!         "secure_password",
//!         Role::User {
//!             allowed_topics: vec![
//!                 TopicPermission::new("chat/*", true, true),
//!                 TopicPermission::new("sensors/#", true, false),
//!             ],
//!         },
//!     ).await.unwrap();
//!
//!     // Authenticate and get a token
//!     let token = auth.login("alice", "secure_password").await.unwrap();
//!
//!     // Validate token and check permissions
//!     let claims = auth.validate_token(&token).unwrap();
//!     assert!(auth.can_publish(&claims.sub, "chat/room1").await);
//!     assert!(!auth.can_publish(&claims.sub, "sensors/temp").await); // read-only
//! }
//! ```
//!
//! # PostgreSQL Support
//!
//! Enable the `postgres` feature for production database support:
//!
//! ```toml
//! popsub_auth = { version = "0.1", features = ["postgres"] }
//! ```
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
//!     let auth = AuthService::with_store_and_admin(config, store).await;
//! }
//! ```

pub mod error;
pub mod jwt;
pub mod permission;
pub mod service;
pub mod store;
pub mod user;

// Re-export main types for convenience
pub use error::AuthError;
pub use jwt::{Claims, JwtService};
pub use permission::{PermissionChecker, TopicPermission};
pub use service::{AuthConfig, AuthService, DefaultUserRole};
pub use store::{InMemoryUserStore, UserStore};
pub use user::{Role, User};

#[cfg(test)]
mod tests;
