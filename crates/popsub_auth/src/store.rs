//! User storage abstraction
//!
//! This module provides an async trait for user storage with implementations:
//! - [`InMemoryUserStore`] - For development and testing
//! - [`PostgresUserStore`] - For production (requires `postgres` feature)
//!
//! # Custom Database Implementation
//!
//! To use your own database backend, implement the [`UserStore`] trait:
//!
//! ```rust,ignore
//! use async_trait::async_trait;
//! use popsub_auth::store::UserStore;
//! use popsub_auth::user::User;
//! use popsub_auth::error::Result;
//!
//! pub struct MyDatabaseStore { /* ... */ }
//!
//! #[async_trait]
//! impl UserStore for MyDatabaseStore {
//!     async fn get_user(&self, username: &str) -> Result<Option<User>> {
//!         // Query your database
//!         todo!()
//!     }
//!     // ... implement other methods
//! }
//!
//! // Then use it with AuthService:
//! use popsub_auth::service::{AuthConfig, AuthService};
//! use std::sync::Arc;
//!
//! let store = Arc::new(MyDatabaseStore { /* ... */ });
//! let config = AuthConfig::default();
//! let auth_service = AuthService::with_store(config, store).await;
//! ```

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use async_trait::async_trait;
use rand::rngs::OsRng;

use crate::error::{AuthError, Result};
use crate::user::{Role, User};

/// Async trait for user storage backends.
///
/// Implementations must be thread-safe (`Send + Sync`).
///
/// # Example
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use popsub_auth::store::UserStore;
///
/// #[async_trait]
/// impl UserStore for MyStore {
///     async fn get_user(&self, username: &str) -> Result<Option<User>> {
///         // Your implementation
///     }
///     // ...
/// }
/// ```
#[async_trait]
pub trait UserStore: Send + Sync {
    /// Get a user by username.
    async fn get_user(&self, username: &str) -> Result<Option<User>>;

    /// Add a new user to the store.
    async fn add_user(&self, user: User) -> Result<()>;

    /// Update an existing user.
    async fn update_user(&self, user: User) -> Result<()>;

    /// Remove a user from the store.
    async fn remove_user(&self, username: &str) -> Result<bool>;

    /// Check if a user exists.
    async fn user_exists(&self, username: &str) -> Result<bool>;

    /// List all usernames.
    async fn list_usernames(&self) -> Result<Vec<String>>;

    /// Get the total number of users.
    async fn user_count(&self) -> Result<usize>;
}

// ============================================================================
// In-Memory Implementation
// ============================================================================

/// In-memory user store implementation.
///
/// Suitable for development and testing. For production, use [`PostgresUserStore`]
/// or implement your own database-backed store.
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
        // Use blocking add for initialization
        {
            let mut users = store
                .users
                .write()
                .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
            users.insert(admin.username.clone(), admin);
        }
        Ok(store)
    }

    /// Create a store with a custom admin user.
    pub fn with_admin(username: &str, password: &str) -> Result<Self> {
        let store = Self::new();
        let password_hash = hash_password(password)?;
        let admin = User::new(username, password_hash, Role::Admin);
        {
            let mut users = store
                .users
                .write()
                .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
            users.insert(admin.username.clone(), admin);
        }
        Ok(store)
    }
}

#[async_trait]
impl UserStore for InMemoryUserStore {
    async fn get_user(&self, username: &str) -> Result<Option<User>> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.get(username).cloned())
    }

    async fn add_user(&self, user: User) -> Result<()> {
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

    async fn update_user(&self, user: User) -> Result<()> {
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

    async fn remove_user(&self, username: &str) -> Result<bool> {
        let mut users = self
            .users
            .write()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.remove(username).is_some())
    }

    async fn user_exists(&self, username: &str) -> Result<bool> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.contains_key(username))
    }

    async fn list_usernames(&self) -> Result<Vec<String>> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.keys().cloned().collect())
    }

    async fn user_count(&self) -> Result<usize> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.len())
    }
}

// ============================================================================
// PostgreSQL Implementation (requires `postgres` feature)
// ============================================================================

#[cfg(feature = "postgres")]
mod postgres_store {
    use super::*;
    use sqlx::Row;
    use sqlx::postgres::PgPool;

    /// PostgreSQL-backed user store for production use.
    ///
    /// # Setup
    ///
    /// 1. Enable the `postgres` feature in your `Cargo.toml`:
    /// ```toml
    /// popsub_auth = { version = "0.1", features = ["postgres"] }
    /// ```
    ///
    /// 2. Run the migrations (see `migrations/` folder)
    ///
    /// 3. Create and use the store:
    /// ```rust,ignore
    /// use sqlx::postgres::PgPoolOptions;
    /// use popsub_auth::store::PostgresUserStore;
    ///
    /// let pool = PgPoolOptions::new()
    ///     .max_connections(5)
    ///     .connect("postgres://user:pass@localhost/popsub")
    ///     .await?;
    ///
    /// let store = PostgresUserStore::new(pool);
    /// store.run_migrations().await?;
    /// ```
    #[derive(Debug, Clone)]
    pub struct PostgresUserStore {
        pool: PgPool,
    }

    impl PostgresUserStore {
        /// Create a new PostgreSQL store with the given connection pool.
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }

        /// Get a reference to the connection pool.
        pub fn pool(&self) -> &PgPool {
            &self.pool
        }

        /// Run database migrations to create the required tables.
        ///
        /// Creates the `users` table if it doesn't exist.
        pub async fn run_migrations(&self) -> Result<()> {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS users (
                    username VARCHAR(255) PRIMARY KEY,
                    password_hash TEXT NOT NULL,
                    role_type VARCHAR(50) NOT NULL,
                    role_data JSONB,
                    enabled BOOLEAN NOT NULL DEFAULT true,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );

                CREATE INDEX IF NOT EXISTS idx_users_enabled ON users(enabled);
                CREATE INDEX IF NOT EXISTS idx_users_role_type ON users(role_type);
                "#,
            )
            .execute(&self.pool)
            .await
            .map_err(|e| AuthError::Internal(format!("Migration failed: {e}")))?;

            Ok(())
        }

        /// Create the store and run migrations.
        pub async fn new_with_migrations(pool: PgPool) -> Result<Self> {
            let store = Self::new(pool);
            store.run_migrations().await?;
            Ok(store)
        }

        /// Serialize role to database format.
        fn serialize_role(role: &Role) -> (String, Option<serde_json::Value>) {
            match role {
                Role::Admin => ("admin".to_string(), None),
                Role::User { allowed_topics } => (
                    "user".to_string(),
                    Some(serde_json::to_value(allowed_topics).unwrap_or_default()),
                ),
                Role::ReadOnly { allowed_topics } => (
                    "readonly".to_string(),
                    Some(serde_json::to_value(allowed_topics).unwrap_or_default()),
                ),
            }
        }

        /// Deserialize role from database format.
        fn deserialize_role(role_type: &str, role_data: Option<serde_json::Value>) -> Result<Role> {
            match role_type {
                "admin" => Ok(Role::Admin),
                "user" => {
                    let allowed_topics = role_data
                        .map(|d| serde_json::from_value(d).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(Role::User { allowed_topics })
                }
                "readonly" => {
                    let allowed_topics = role_data
                        .map(|d| serde_json::from_value(d).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(Role::ReadOnly { allowed_topics })
                }
                _ => Err(AuthError::Internal(format!(
                    "Unknown role type: {role_type}"
                ))),
            }
        }
    }

    #[async_trait]
    impl UserStore for PostgresUserStore {
        async fn get_user(&self, username: &str) -> Result<Option<User>> {
            let row = sqlx::query(
                r#"
                SELECT username, password_hash, role_type, role_data, enabled
                FROM users
                WHERE username = $1
                "#,
            )
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            match row {
                Some(row) => {
                    let username: String = row.get("username");
                    let password_hash: String = row.get("password_hash");
                    let role_type: String = row.get("role_type");
                    let role_data: Option<serde_json::Value> = row.get("role_data");
                    let enabled: bool = row.get("enabled");

                    let role = Self::deserialize_role(&role_type, role_data)?;
                    let mut user = User::new(&username, password_hash, role);
                    user.enabled = enabled;

                    Ok(Some(user))
                }
                None => Ok(None),
            }
        }

        async fn add_user(&self, user: User) -> Result<()> {
            let (role_type, role_data) = Self::serialize_role(&user.role);

            sqlx::query(
                r#"
                INSERT INTO users (username, password_hash, role_type, role_data, enabled)
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(&user.username)
            .bind(&user.password_hash)
            .bind(&role_type)
            .bind(&role_data)
            .bind(user.enabled)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                if e.to_string().contains("duplicate key") || e.to_string().contains("unique") {
                    AuthError::UserAlreadyExists(user.username.clone())
                } else {
                    AuthError::Internal(format!("Database error: {e}"))
                }
            })?;

            Ok(())
        }

        async fn update_user(&self, user: User) -> Result<()> {
            let (role_type, role_data) = Self::serialize_role(&user.role);

            let result = sqlx::query(
                r#"
                UPDATE users
                SET password_hash = $2, role_type = $3, role_data = $4, enabled = $5, updated_at = NOW()
                WHERE username = $1
                "#,
            )
            .bind(&user.username)
            .bind(&user.password_hash)
            .bind(&role_type)
            .bind(&role_data)
            .bind(user.enabled)
            .execute(&self.pool)
            .await
            .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            if result.rows_affected() == 0 {
                return Err(AuthError::UserNotFound(user.username));
            }

            Ok(())
        }

        async fn remove_user(&self, username: &str) -> Result<bool> {
            let result = sqlx::query("DELETE FROM users WHERE username = $1")
                .bind(username)
                .execute(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            Ok(result.rows_affected() > 0)
        }

        async fn user_exists(&self, username: &str) -> Result<bool> {
            let row = sqlx::query("SELECT 1 FROM users WHERE username = $1")
                .bind(username)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            Ok(row.is_some())
        }

        async fn list_usernames(&self) -> Result<Vec<String>> {
            let rows = sqlx::query("SELECT username FROM users ORDER BY username")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            Ok(rows.iter().map(|r| r.get("username")).collect())
        }

        async fn user_count(&self) -> Result<usize> {
            let row = sqlx::query("SELECT COUNT(*) as count FROM users")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            let count: i64 = row.get("count");
            Ok(count as usize)
        }
    }
}

#[cfg(feature = "postgres")]
pub use postgres_store::PostgresUserStore;

// ============================================================================
// Password Hashing Utilities
// ============================================================================

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

// ============================================================================
// Tests
// ============================================================================

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

    #[tokio::test]
    async fn test_add_and_get_user() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let user = User::new("testuser", hash, Role::Admin);

        store.add_user(user).await.unwrap();

        let retrieved = store.get_user("testuser").await.unwrap().unwrap();
        assert_eq!(retrieved.username, "testuser");
        assert!(retrieved.is_admin());
    }

    #[tokio::test]
    async fn test_duplicate_user() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let user1 = User::new("duplicate", hash.clone(), Role::Admin);
        let user2 = User::new("duplicate", hash, Role::Admin);

        store.add_user(user1).await.unwrap();
        let result = store.add_user(user2).await;

        assert!(matches!(result, Err(AuthError::UserAlreadyExists(_))));
    }

    #[tokio::test]
    async fn test_remove_user() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let user = User::new("to_remove", hash, Role::Admin);

        store.add_user(user).await.unwrap();
        assert!(store.user_exists("to_remove").await.unwrap());

        store.remove_user("to_remove").await.unwrap();
        assert!(!store.user_exists("to_remove").await.unwrap());
    }

    #[test]
    fn test_with_default_admin() {
        let store = InMemoryUserStore::with_default_admin().unwrap();

        // Use blocking access for sync test
        let users = store.users.read().unwrap();
        let admin = users.get("admin").unwrap();
        assert!(admin.is_admin());
        assert!(verify_password("password", &admin.password_hash).unwrap());
    }

    #[tokio::test]
    async fn test_user_with_topic_permissions() {
        let store = InMemoryUserStore::new();

        let hash = hash_password("test123").unwrap();
        let role = Role::User {
            allowed_topics: vec![
                TopicPermission::full_access("chat/#"),
                TopicPermission::read_only("sensors/*"),
            ],
        };
        let user = User::new("limited_user", hash, role);

        store.add_user(user).await.unwrap();

        let retrieved = store.get_user("limited_user").await.unwrap().unwrap();
        if let Role::User { allowed_topics } = &retrieved.role {
            assert_eq!(allowed_topics.len(), 2);
        } else {
            panic!("Expected User role");
        }
    }
}
