//! User storage abstraction
//!
//! This module provides a trait for user storage and an in-memory implementation.
//! Additional implementations (e.g., database-backed) can be added by implementing
//! the `UserStore` trait.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use rand::rngs::OsRng;

use crate::error::{AuthError, Result};
use crate::user::{Role, User};

/// Trait for user storage backends.
///
/// Implementations must be thread-safe (`Send + Sync`).
pub trait UserStore: Send + Sync {
    /// Get a user by username.
    fn get_user(&self, username: &str) -> Result<Option<User>>;

    /// Add a new user to the store.
    fn add_user(&self, user: User) -> Result<()>;

    /// Update an existing user.
    fn update_user(&self, user: User) -> Result<()>;

    /// Remove a user from the store.
    fn remove_user(&self, username: &str) -> Result<bool>;

    /// Check if a user exists.
    fn user_exists(&self, username: &str) -> Result<bool>;

    /// List all usernames.
    fn list_usernames(&self) -> Result<Vec<String>>;

    /// Get the total number of users.
    fn user_count(&self) -> Result<usize>;
}

/// In-memory user store implementation.
///
/// Suitable for development and testing. For production, consider implementing
/// a database-backed store.
#[derive(Debug)]
pub struct InMemoryUserStore {
    users: Arc<RwLock<HashMap<String, User>>>,
}

impl Default for InMemoryUserStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryUserStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a store pre-populated with a default admin user.
    ///
    /// The admin user has username "admin" and password "password".
    /// **This is for development only - change credentials in production!**
    pub fn with_default_admin() -> Result<Self> {
        let store = Self::new();
        let password_hash = hash_password("password")?;
        let admin = User::new("admin", password_hash, Role::Admin);
        store.add_user(admin)?;
        Ok(store)
    }

    /// Create a store with a custom admin user.
    pub fn with_admin(username: &str, password: &str) -> Result<Self> {
        let store = Self::new();
        let password_hash = hash_password(password)?;
        let admin = User::new(username, password_hash, Role::Admin);
        store.add_user(admin)?;
        Ok(store)
    }
}

impl UserStore for InMemoryUserStore {
    fn get_user(&self, username: &str) -> Result<Option<User>> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.get(username).cloned())
    }

    fn add_user(&self, user: User) -> Result<()> {
        let mut users = self
            .users
            .write()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;

        if users.contains_key(&user.username) {
            return Err(AuthError::UserAlreadyExists(user.username));
        }

        users.insert(user.username.clone(), user);
        Ok(())
    }

    fn update_user(&self, user: User) -> Result<()> {
        let mut users = self
            .users
            .write()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;

        if !users.contains_key(&user.username) {
            return Err(AuthError::UserNotFound(user.username));
        }

        users.insert(user.username.clone(), user);
        Ok(())
    }

    fn remove_user(&self, username: &str) -> Result<bool> {
        let mut users = self
            .users
            .write()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.remove(username).is_some())
    }

    fn user_exists(&self, username: &str) -> Result<bool> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.contains_key(username))
    }

    fn list_usernames(&self) -> Result<Vec<String>> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.keys().cloned().collect())
    }

    fn user_count(&self) -> Result<usize> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.len())
    }
}

/// Hash a password using Argon2.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AuthError::PasswordHashError)?;

    Ok(hash.to_string())
}

/// Verify a password against a hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed_hash = PasswordHash::new(hash).map_err(|_| AuthError::PasswordHashError)?;

    let argon2 = Argon2::default();
    Ok(argon2
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::permission::TopicPermission;

    #[test]
    fn test_hash_and_verify_password() {
        let password = "secure_password123";
        let hash = hash_password(password).unwrap();

        assert!(verify_password(password, &hash).unwrap());
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_add_and_get_user() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let user = User::new("testuser", hash, Role::Admin);

        store.add_user(user).unwrap();

        let retrieved = store.get_user("testuser").unwrap().unwrap();
        assert_eq!(retrieved.username, "testuser");
        assert!(retrieved.is_admin());
    }

    #[test]
    fn test_duplicate_user() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let user1 = User::new("duplicate", hash.clone(), Role::Admin);
        let user2 = User::new("duplicate", hash, Role::Admin);

        store.add_user(user1).unwrap();
        let result = store.add_user(user2);

        assert!(matches!(result, Err(AuthError::UserAlreadyExists(_))));
    }

    #[test]
    fn test_remove_user() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let user = User::new("to_remove", hash, Role::Admin);

        store.add_user(user).unwrap();
        assert!(store.user_exists("to_remove").unwrap());

        store.remove_user("to_remove").unwrap();
        assert!(!store.user_exists("to_remove").unwrap());
    }

    #[test]
    fn test_with_default_admin() {
        let store = InMemoryUserStore::with_default_admin().unwrap();

        let admin = store.get_user("admin").unwrap().unwrap();
        assert!(admin.is_admin());
        assert!(verify_password("password", &admin.password_hash).unwrap());
    }

    #[test]
    fn test_user_with_topic_permissions() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let role = Role::User {
            allowed_topics: vec![
                TopicPermission::full_access("chat/#"),
                TopicPermission::read_only("sensors/*"),
            ],
        };
        let user = User::new("limited_user", hash, role);

        store.add_user(user).unwrap();

        let retrieved = store.get_user("limited_user").unwrap().unwrap();
        if let Role::User { allowed_topics } = &retrieved.role {
            assert_eq!(allowed_topics.len(), 2);
        } else {
            panic!("Expected User role");
        }
    }
}
