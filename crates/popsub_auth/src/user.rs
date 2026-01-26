//! User and Role definitions
//!
//! This module defines the core user model and role-based access control types.

use serde::{Deserialize, Serialize};

use crate::permission::TopicPermission;

/// A user in the system with credentials and role information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Unique username (used as the primary identifier).
    pub username: String,
    /// Argon2 hashed password.
    pub password_hash: String,
    /// The user's role determining their permissions.
    pub role: Role,
    /// Whether the user account is active.
    pub enabled: bool,
    /// Unix timestamp (seconds) when the user was created.
    pub created_at: i64,
}

impl User {
    /// Create a new user with the given credentials and role.
    ///
    /// The password should already be hashed before calling this constructor.
    pub fn new(username: impl Into<String>, password_hash: impl Into<String>, role: Role) -> Self {
        Self {
            username: username.into(),
            password_hash: password_hash.into(),
            role,
            enabled: true,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Check if the user has admin privileges.
    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }
}

/// User roles with associated permissions.
///
/// # Role Hierarchy
///
/// - `Admin`: Full access to all topics and system operations
/// - `User`: Access limited to explicitly allowed topics
/// - `ReadOnly`: Can only subscribe (no publishing)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Role {
    /// Administrator with full access to all topics.
    #[serde(rename = "admin")]
    Admin,

    /// Regular user with topic-specific permissions.
    #[serde(rename = "user")]
    User {
        /// List of topic permissions for this user.
        allowed_topics: Vec<TopicPermission>,
    },

    /// Read-only user who can only subscribe to specified topics.
    #[serde(rename = "readonly")]
    ReadOnly {
        /// List of topics this user can subscribe to.
        allowed_topics: Vec<String>,
    },
}

impl Default for Role {
    fn default() -> Self {
        // Default to a user with no topic permissions
        Role::User {
            allowed_topics: Vec::new(),
        }
    }
}

impl Role {
    /// Create an admin role.
    pub fn admin() -> Self {
        Role::Admin
    }

    /// Create a user role with the given topic permissions.
    pub fn user(permissions: Vec<TopicPermission>) -> Self {
        Role::User {
            allowed_topics: permissions,
        }
    }

    /// Create a read-only role with the given allowed topics.
    pub fn read_only(topics: Vec<String>) -> Self {
        Role::ReadOnly {
            allowed_topics: topics,
        }
    }

    /// Create a user role with full access to all topics (wildcard).
    pub fn user_full_access() -> Self {
        Role::User {
            allowed_topics: vec![TopicPermission::new("#", true, true)],
        }
    }
}
