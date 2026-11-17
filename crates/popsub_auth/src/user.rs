//! The `User` record kept by every `UserStore` and the `Role` that decides its topic access.

use serde::{Deserialize, Serialize};

use crate::permission::TopicPermission;

/// One account: its username, Argon2 password hash, role and enabled flag.
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
    /// Which generation of tokens is valid. A token carries the version it
    /// was issued under and is refused once this has moved on, which is how
    /// a password change cancels every token issued before it.
    #[serde(default)]
    pub token_version: u64,
}

impl User {
    /// Builds an enabled user stamped with the current time as `created_at`.
    ///
    /// The constructor stores `password_hash` as given and does not hash it, so
    /// callers pass the output of `store::hash_password`.
    ///
    /// Args:
    ///     username: The unique username.
    ///     password_hash: An Argon2 PHC string for the user's password.
    ///     role: The role that decides which topics the user can use.
    ///
    /// Returns:
    ///     The new user with `enabled` set to `true`.
    pub fn new(username: impl Into<String>, password_hash: impl Into<String>, role: Role) -> Self {
        Self {
            username: username.into(),
            password_hash: password_hash.into(),
            role,
            enabled: true,
            created_at: chrono::Utc::now().timestamp(),
            token_version: 0,
        }
    }

    /// Reports whether the user's role is `Role::Admin`.
    ///
    /// Returns:
    ///     `true` for admins, `false` for every other role.
    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }
}

/// What a user may do, serialized with a `type` tag of `admin`, `user` or `readonly`.
///
/// `Admin` can subscribe and publish on every topic. `User` can do what its
/// `TopicPermission` list allows. `ReadOnly` can subscribe to the listed
/// patterns and can never publish.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Role {
    /// Subscribe and publish on every topic.
    #[serde(rename = "admin")]
    Admin,

    /// Access limited to the topics granted in `allowed_topics`.
    #[serde(rename = "user")]
    User {
        /// Patterns with their subscribe and publish flags.
        allowed_topics: Vec<TopicPermission>,
    },

    /// Subscribe-only access to the listed topic patterns.
    #[serde(rename = "readonly")]
    ReadOnly {
        /// Topic patterns the user may subscribe to.
        allowed_topics: Vec<String>,
    },
}

impl Default for Role {
    /// Builds a `Role::User` with no topic permissions, so the user can do nothing until granted topics.
    ///
    /// Returns:
    ///     `Role::User` with an empty `allowed_topics` list.
    fn default() -> Self {
        Role::User {
            allowed_topics: Vec::new(),
        }
    }
}

impl Role {
    /// Builds `Role::Admin`.
    ///
    /// Returns:
    ///     The admin role.
    pub fn admin() -> Self {
        Role::Admin
    }

    /// Builds `Role::User` from a list of topic permissions.
    ///
    /// Args:
    ///     permissions: The patterns and flags the user is granted.
    ///
    /// Returns:
    ///     `Role::User` holding `permissions`.
    pub fn user(permissions: Vec<TopicPermission>) -> Self {
        Role::User {
            allowed_topics: permissions,
        }
    }

    /// Builds `Role::ReadOnly` from a list of topic patterns.
    ///
    /// Args:
    ///     topics: Patterns the user may subscribe to.
    ///
    /// Returns:
    ///     `Role::ReadOnly` holding `topics`.
    pub fn read_only(topics: Vec<String>) -> Self {
        Role::ReadOnly {
            allowed_topics: topics,
        }
    }

    /// Builds `Role::User` with one `#` permission that allows subscribe and publish on every topic.
    ///
    /// Unlike `Role::Admin`, the result still reports `is_admin() == false`.
    ///
    /// Returns:
    ///     `Role::User` with a single full-access `#` pattern.
    pub fn user_full_access() -> Self {
        Role::User {
            allowed_topics: vec![TopicPermission::new("#", true, true)],
        }
    }
}
