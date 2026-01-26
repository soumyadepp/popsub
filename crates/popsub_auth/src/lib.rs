//! popsub_auth
//!
//! Authentication and authorization module for PopSub. This crate provides:
//!
//! - **JWT token generation and validation** via the `JwtService`
//! - **User management** with the `User` struct and `Role`-based permissions
//! - **Role-Based Access Control (RBAC)** for topic-level authorization
//! - **Pluggable user storage** via the `UserStore` trait
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
//! ```rust,no_run
//! use popsub_auth::{AuthService, AuthConfig, Role, TopicPermission};
//!
//! // Create auth service with default in-memory store
//! let config = AuthConfig::default();
//! let mut auth = AuthService::new(config);
//!
//! // Add a user with publish/subscribe permissions
//! auth.add_user(
//!     "alice",
//!     "secure_password",
//!     Role::User {
//!         allowed_topics: vec![
//!             TopicPermission::new("chat/*", true, true),
//!             TopicPermission::new("sensors/#", true, false),
//!         ],
//!     },
//! ).unwrap();
//!
//! // Authenticate and get a token
//! let token = auth.login("alice", "secure_password").unwrap();
//!
//! // Validate token and check permissions
//! let claims = auth.validate_token(&token).unwrap();
//! assert!(auth.can_publish(&claims.sub, "chat/room1"));
//! assert!(!auth.can_publish(&claims.sub, "sensors/temp")); // read-only
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
