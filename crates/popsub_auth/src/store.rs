//! The `UserStore` trait, its in-memory and PostgreSQL implementations, and Argon2 password hashing.
//!
//! `InMemoryUserStore` is the default and loses every account on restart.
//! `FileUserStore` (in `file_store`) keeps accounts in a JSON file.
//! `PostgresUserStore` is compiled only with the `postgres` feature. Any other
//! backend can be plugged into `AuthService::with_store` by implementing
//! [`UserStore`].
//!
//! Example:
//!
//! ```rust,ignore
//! use async_trait::async_trait;
//! use popsub_auth::store::UserStore;
//! use popsub_auth::user::User;
//! use popsub_auth::error::Result;
//! use popsub_auth::service::{AuthConfig, AuthService};
//! use std::sync::Arc;
//!
//! pub struct MyDatabaseStore;
//!
//! #[async_trait]
//! impl UserStore for MyDatabaseStore {
//!     async fn get_user(&self, username: &str) -> Result<Option<User>> {
//!         todo!()
//!     }
//! }
//!
//! let store = Arc::new(MyDatabaseStore);
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

/// Where `AuthService` keeps accounts, keyed by username.
///
/// Implementations are shared across tasks, so they must be `Send + Sync`.
/// Backends report their own failures as `AuthError::Internal`.
///
/// Example:
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use popsub_auth::store::UserStore;
///
/// #[async_trait]
/// impl UserStore for MyStore {
///     async fn get_user(&self, username: &str) -> Result<Option<User>> {
///         todo!()
///     }
/// }
/// ```
#[async_trait]
pub trait UserStore: Send + Sync {
    /// Looks up one account.
    ///
    /// Args:
    ///     username: The account to fetch.
    ///
    /// Returns:
    ///     The user, or `None` when there is no such account.
    ///
    /// Raises:
    ///     AuthError::Internal: When the backend fails.
    async fn get_user(&self, username: &str) -> Result<Option<User>>;

    /// Inserts a new account, refusing to overwrite an existing one.
    ///
    /// Args:
    ///     user: The account to insert.
    ///
    /// Raises:
    ///     AuthError::UserAlreadyExists: When the username is taken.
    ///     AuthError::Internal: When the backend fails.
    async fn add_user(&self, user: User) -> Result<()>;

    /// Replaces an existing account, matched by `user.username`.
    ///
    /// Args:
    ///     user: The new state of the account.
    ///
    /// Raises:
    ///     AuthError::UserNotFound: When there is no such account.
    ///     AuthError::Internal: When the backend fails.
    async fn update_user(&self, user: User) -> Result<()>;

    /// Deletes an account if it exists.
    ///
    /// Args:
    ///     username: The account to delete.
    ///
    /// Returns:
    ///     `true` if an account was deleted, `false` if none existed.
    ///
    /// Raises:
    ///     AuthError::Internal: When the backend fails.
    async fn remove_user(&self, username: &str) -> Result<bool>;

    /// Reports whether an account exists.
    ///
    /// Args:
    ///     username: The username to look for.
    ///
    /// Returns:
    ///     `true` if the account exists.
    ///
    /// Raises:
    ///     AuthError::Internal: When the backend fails.
    async fn user_exists(&self, username: &str) -> Result<bool>;

    /// Lists every username in the store, in an order the backend chooses.
    ///
    /// Returns:
    ///     All usernames.
    ///
    /// Raises:
    ///     AuthError::Internal: When the backend fails.
    async fn list_usernames(&self) -> Result<Vec<String>>;

    /// Counts the accounts in the store.
    ///
    /// Returns:
    ///     The number of accounts.
    ///
    /// Raises:
    ///     AuthError::Internal: When the backend fails.
    async fn user_count(&self) -> Result<usize>;
}

/// A `UserStore` backed by a `HashMap` behind a `std::sync::RwLock`.
///
/// Accounts live only as long as the process. Locks are never held across an
/// `.await`. Use `PostgresUserStore` or another backend when accounts must
/// survive a restart.
#[derive(Debug)]
pub struct InMemoryUserStore {
    users: Arc<RwLock<HashMap<String, User>>>,
}

impl Default for InMemoryUserStore {
    /// Builds an empty store, same as `InMemoryUserStore::new`.
    ///
    /// Returns:
    ///     A store with no accounts.
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryUserStore {
    /// Builds an empty store.
    ///
    /// Returns:
    ///     A store with no accounts.
    pub fn new() -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Builds a store holding one admin with username `admin` and password `password`.
    ///
    /// Those credentials are published with PopSub, so this is for local
    /// development only. The admin is inserted through the lock directly
    /// because this constructor is synchronous.
    ///
    /// Returns:
    ///     The store with the admin account.
    ///
    /// Raises:
    ///     AuthError::PasswordHashError: When hashing the password fails.
    ///     AuthError::Internal: When the store's lock is poisoned.
    pub fn with_default_admin() -> Result<Self> {
        let store = Self::new();
        let password_hash = hash_password("password")?;
        let admin = User::new("admin", password_hash, Role::Admin);
        {
            let mut users = store
                .users
                .write()
                .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
            users.insert(admin.username.clone(), admin);
        }
        Ok(store)
    }

    /// Builds a store holding one admin with the given credentials.
    ///
    /// Args:
    ///     username: The admin's username.
    ///     password: The admin's plain text password, hashed before it is stored.
    ///
    /// Returns:
    ///     The store with the admin account.
    ///
    /// Raises:
    ///     AuthError::PasswordHashError: When hashing the password fails.
    ///     AuthError::Internal: When the store's lock is poisoned.
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
    /// Clones the account out of the map under a read lock.
    ///
    /// Args:
    ///     username: The account to fetch.
    ///
    /// Returns:
    ///     A copy of the user, or `None` when there is no such account.
    ///
    /// Raises:
    ///     AuthError::Internal: When the store's lock is poisoned.
    async fn get_user(&self, username: &str) -> Result<Option<User>> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.get(username).cloned())
    }

    /// Inserts the account under a write lock unless the username is taken.
    ///
    /// Args:
    ///     user: The account to insert.
    ///
    /// Raises:
    ///     AuthError::UserAlreadyExists: When the username is taken.
    ///     AuthError::Internal: When the store's lock is poisoned.
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

    /// Overwrites an existing account under a write lock.
    ///
    /// Args:
    ///     user: The new state of the account.
    ///
    /// Raises:
    ///     AuthError::UserNotFound: When there is no such account.
    ///     AuthError::Internal: When the store's lock is poisoned.
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

    /// Removes the account from the map under a write lock.
    ///
    /// Args:
    ///     username: The account to delete.
    ///
    /// Returns:
    ///     `true` if an account was deleted, `false` if none existed.
    ///
    /// Raises:
    ///     AuthError::Internal: When the store's lock is poisoned.
    async fn remove_user(&self, username: &str) -> Result<bool> {
        let mut users = self
            .users
            .write()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.remove(username).is_some())
    }

    /// Checks the map for the username under a read lock.
    ///
    /// Args:
    ///     username: The username to look for.
    ///
    /// Returns:
    ///     `true` if the account exists.
    ///
    /// Raises:
    ///     AuthError::Internal: When the store's lock is poisoned.
    async fn user_exists(&self, username: &str) -> Result<bool> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.contains_key(username))
    }

    /// Copies the map's keys under a read lock.
    ///
    /// Returns:
    ///     All usernames, in no particular order.
    ///
    /// Raises:
    ///     AuthError::Internal: When the store's lock is poisoned.
    async fn list_usernames(&self) -> Result<Vec<String>> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.keys().cloned().collect())
    }

    /// Reads the map's length under a read lock.
    ///
    /// Returns:
    ///     The number of accounts.
    ///
    /// Raises:
    ///     AuthError::Internal: When the store's lock is poisoned.
    async fn user_count(&self) -> Result<usize> {
        let users = self
            .users
            .read()
            .map_err(|_| AuthError::Internal("lock poisoned".into()))?;
        Ok(users.len())
    }
}

#[cfg(feature = "postgres")]
mod postgres_store {
    use super::*;
    use sqlx::Row;
    use sqlx::postgres::PgPool;

    /// The schema `run_migrations` creates, one statement per entry.
    ///
    /// Postgres refuses several commands in one prepared statement, which is
    /// how `sqlx::query` sends SQL, so each statement is executed on its own.
    const MIGRATION_STATEMENTS: [&str; 3] = [
        r#"
        CREATE TABLE IF NOT EXISTS users (
            username VARCHAR(255) PRIMARY KEY,
            password_hash TEXT NOT NULL,
            role_type VARCHAR(50) NOT NULL,
            role_data JSONB,
            enabled BOOLEAN NOT NULL DEFAULT true,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
        "CREATE INDEX IF NOT EXISTS idx_users_enabled ON users(enabled)",
        "CREATE INDEX IF NOT EXISTS idx_users_role_type ON users(role_type)",
    ];

    /// A `UserStore` backed by a PostgreSQL `users` table, available with the `postgres` feature.
    ///
    /// Roles are stored as a `role_type` string plus a JSONB `role_data`
    /// column holding the role's topic list. Create the table with
    /// `run_migrations` or `migrations/001_create_users_table.sql` before use.
    /// CI compiles this store but does not run it against a database.
    ///
    /// Example:
    ///
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
        /// Wraps a connection pool without touching the database.
        ///
        /// Args:
        ///     pool: The pool to run queries on.
        ///
        /// Returns:
        ///     The store.
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }

        /// Exposes the connection pool so callers can run their own queries.
        ///
        /// Returns:
        ///     The store's pool.
        pub fn pool(&self) -> &PgPool {
            &self.pool
        }

        /// Creates the `users` table and its two indexes if they are missing.
        ///
        /// Unlike the SQL file in `migrations/`, this does not install the
        /// trigger that keeps `updated_at` current; `update_user` sets it instead.
        ///
        /// Raises:
        ///     AuthError::Internal: When the statements fail.
        pub async fn run_migrations(&self) -> Result<()> {
            for statement in MIGRATION_STATEMENTS {
                sqlx::query(statement)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| AuthError::Internal(format!("Migration failed: {e}")))?;
            }
            Ok(())
        }

        /// Wraps a connection pool and runs `run_migrations` on it.
        ///
        /// Args:
        ///     pool: The pool to run queries on.
        ///
        /// Returns:
        ///     The store, with the table in place.
        ///
        /// Raises:
        ///     AuthError::Internal: When the migration fails.
        pub async fn new_with_migrations(pool: PgPool) -> Result<Self> {
            let store = Self::new(pool);
            store.run_migrations().await?;
            Ok(store)
        }

        /// Splits a role into its `role_type` column value and its JSONB `role_data`.
        ///
        /// A topic list that fails to serialize is stored as JSON `null`.
        ///
        /// Args:
        ///     role: The role to store.
        ///
        /// Returns:
        ///     The role type (`admin`, `user` or `readonly`) and the topic list
        ///     as JSON, or `None` for admins.
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

        /// Rebuilds a role from its `role_type` and `role_data` columns.
        ///
        /// A missing `role_data` gives an empty topic list.
        ///
        /// Args:
        ///     role_type: The stored role type.
        ///     role_data: The stored topic list, if any.
        ///
        /// Returns:
        ///     The role.
        ///
        /// Raises:
        ///     AuthError::Internal: When `role_data` does not fit the role or the
        ///         role type is unknown.
        fn deserialize_role(role_type: &str, role_data: Option<serde_json::Value>) -> Result<Role> {
            match role_type {
                "admin" => Ok(Role::Admin),
                "user" => {
                    let allowed_topics = match role_data {
                        Some(d) => serde_json::from_value(d).map_err(|e| {
                            AuthError::Internal(format!(
                                "Failed to deserialize role data for user role: {e}"
                            ))
                        })?,
                        None => Vec::new(),
                    };
                    Ok(Role::User { allowed_topics })
                }
                "readonly" => {
                    let allowed_topics = match role_data {
                        Some(d) => serde_json::from_value(d).map_err(|e| {
                            AuthError::Internal(format!(
                                "Failed to deserialize role data for readonly role: {e}"
                            ))
                        })?,
                        None => Vec::new(),
                    };
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
        /// Selects one row by username and turns it into a `User`.
        ///
        /// `created_at` is read from the row as seconds since the UNIX epoch.
        ///
        /// Args:
        ///     username: The account to fetch.
        ///
        /// Returns:
        ///     The user, or `None` when there is no such row.
        ///
        /// Raises:
        ///     AuthError::Internal: When the query fails or the stored role
        ///         cannot be decoded.
        async fn get_user(&self, username: &str) -> Result<Option<User>> {
            let row = sqlx::query(
                r#"
                SELECT username, password_hash, role_type, role_data, enabled,
                       EXTRACT(EPOCH FROM created_at)::BIGINT AS created_at
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
                    user.created_at = row.get("created_at");

                    Ok(Some(user))
                }
                None => Ok(None),
            }
        }

        /// Inserts a row for the account. `created_at` is set by the database.
        ///
        /// A duplicate username is detected from the database's unique
        /// violation error code, not from the message text.
        ///
        /// Args:
        ///     user: The account to insert.
        ///
        /// Raises:
        ///     AuthError::UserAlreadyExists: When the username is taken.
        ///     AuthError::Internal: When the query fails.
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
                if e.as_database_error()
                    .is_some_and(|db| db.is_unique_violation())
                {
                    AuthError::UserAlreadyExists(user.username.clone())
                } else {
                    AuthError::Internal(format!("Database error: {e}"))
                }
            })?;

            Ok(())
        }

        /// Updates the account's hash, role and enabled flag and sets `updated_at` to now.
        ///
        /// Args:
        ///     user: The new state of the account.
        ///
        /// Raises:
        ///     AuthError::UserNotFound: When no row has this username.
        ///     AuthError::Internal: When the query fails.
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

        /// Deletes the account's row.
        ///
        /// Args:
        ///     username: The account to delete.
        ///
        /// Returns:
        ///     `true` if a row was deleted, `false` if none matched.
        ///
        /// Raises:
        ///     AuthError::Internal: When the query fails.
        async fn remove_user(&self, username: &str) -> Result<bool> {
            let result = sqlx::query("DELETE FROM users WHERE username = $1")
                .bind(username)
                .execute(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            Ok(result.rows_affected() > 0)
        }

        /// Checks for a row with this username.
        ///
        /// Args:
        ///     username: The username to look for.
        ///
        /// Returns:
        ///     `true` if the row exists.
        ///
        /// Raises:
        ///     AuthError::Internal: When the query fails.
        async fn user_exists(&self, username: &str) -> Result<bool> {
            let row = sqlx::query("SELECT 1 FROM users WHERE username = $1")
                .bind(username)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            Ok(row.is_some())
        }

        /// Selects every username, sorted alphabetically.
        ///
        /// Returns:
        ///     All usernames in ascending order.
        ///
        /// Raises:
        ///     AuthError::Internal: When the query fails.
        async fn list_usernames(&self) -> Result<Vec<String>> {
            let rows = sqlx::query("SELECT username FROM users ORDER BY username")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| AuthError::Internal(format!("Database error: {e}")))?;

            Ok(rows.iter().map(|r| r.get("username")).collect())
        }

        /// Counts the rows in the `users` table.
        ///
        /// Returns:
        ///     The number of accounts.
        ///
        /// Raises:
        ///     AuthError::Internal: When the query fails.
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

/// Hashes a password with Argon2id (the `argon2` crate's defaults) and a random salt.
///
/// Args:
///     password: The plain text password.
///
/// Returns:
///     The hash as a PHC string, which embeds the salt and parameters.
///
/// Raises:
///     AuthError::PasswordHashError: When hashing fails.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AuthError::PasswordHashError)?;

    Ok(hash.to_string())
}

/// Checks a plain text password against a stored PHC hash string.
///
/// Args:
///     password: The password to check.
///     hash: The stored hash from `hash_password`.
///
/// Returns:
///     `true` when the password matches, `false` when it does not.
///
/// Raises:
///     AuthError::PasswordHashError: When `hash` is not a valid PHC string.
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

    /// Checks that a hashed password verifies and a different password does not.
    #[test]
    fn test_hash_and_verify_password() {
        let password = "secure_password123";
        let hash = hash_password(password).unwrap();

        assert!(verify_password(password, &hash).unwrap());
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    /// Checks that an added user can be read back with its role intact.
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

    /// Checks that adding a second user with the same username returns `UserAlreadyExists`.
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

    /// Checks that a removed user no longer exists in the store.
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

    /// Checks that `with_default_admin` stores an admin whose password is `password`.
    #[test]
    fn test_with_default_admin() {
        let store = InMemoryUserStore::with_default_admin().unwrap();

        let users = store.users.read().unwrap();
        let admin = users.get("admin").unwrap();
        assert!(admin.is_admin());
        assert!(verify_password("password", &admin.password_hash).unwrap());
    }

    /// Checks that a user role keeps all of its topic permissions through a store round trip.
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
