//! Authentication service
//!
//! The main entry point for authentication and authorization operations.
//! Combines JWT handling, user storage, and permission checking.

use std::sync::Arc;

use tracing::{info, warn};

use crate::error::{AuthError, Result};
use crate::jwt::{Claims, JwtService};
use crate::permission::{PermissionChecker, TopicPermission};
use crate::store::{InMemoryUserStore, UserStore, hash_password, verify_password};
use crate::user::{Role, User};

/// Configuration for the authentication service.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Secret key for JWT signing.
    pub jwt_secret: String,
    /// JWT token expiration time in hours.
    pub jwt_expiration_hours: u64,
    /// Default admin username (created on startup if store is empty).
    pub default_admin_username: String,
    /// Default admin password.
    pub default_admin_password: String,
    /// Whether to allow public user registration.
    pub allow_registration: bool,
    /// Default role for newly registered users.
    pub default_user_role: DefaultUserRole,
}

/// Default role configuration for new user registrations.
#[derive(Debug, Clone)]
pub enum DefaultUserRole {
    /// Full access to all topics (subscribe + publish).
    FullAccess,
    /// Read-only access to all topics.
    ReadOnly,
    /// Access to specific topic patterns.
    LimitedAccess(Vec<TopicPermission>),
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: "change_me_in_production".to_string(),
            jwt_expiration_hours: 24,
            default_admin_username: "admin".to_string(),
            default_admin_password: "password".to_string(),
            allow_registration: true,
            default_user_role: DefaultUserRole::FullAccess,
        }
    }
}

impl AuthConfig {
    /// Create a new config with the given JWT secret.
    pub fn new(jwt_secret: impl Into<String>) -> Self {
        Self {
            jwt_secret: jwt_secret.into(),
            ..Default::default()
        }
    }

    /// Set the JWT expiration hours.
    pub fn with_expiration(mut self, hours: u64) -> Self {
        self.jwt_expiration_hours = hours;
        self
    }

    /// Set the default admin credentials.
    pub fn with_admin(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.default_admin_username = username.into();
        self.default_admin_password = password.into();
        self
    }

    /// Enable or disable public user registration.
    pub fn with_registration(mut self, enabled: bool) -> Self {
        self.allow_registration = enabled;
        self
    }

    /// Set the default role for newly registered users.
    pub fn with_default_role(mut self, role: DefaultUserRole) -> Self {
        self.default_user_role = role;
        self
    }
}

/// Main authentication service.
///
/// Provides login, token validation, and authorization operations.
pub struct AuthService {
    jwt_service: JwtService,
    /// The user store backend. Public for testing purposes.
    pub user_store: Arc<dyn UserStore>,
    config: AuthConfig,
}

impl std::fmt::Debug for AuthService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthService")
            .field("jwt_service", &self.jwt_service)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AuthService {
    /// Create a new auth service with the given configuration.
    ///
    /// Uses an in-memory user store and creates a default admin user.
    pub fn new(config: AuthConfig) -> Self {
        let jwt_service = JwtService::new(&config.jwt_secret, config.jwt_expiration_hours);
        let user_store = Arc::new(InMemoryUserStore::new());

        let mut service = Self {
            jwt_service,
            user_store,
            config: config.clone(),
        };

        // Create default admin user
        if let Err(e) = service.add_user(
            &config.default_admin_username,
            &config.default_admin_password,
            Role::Admin,
        ) {
            warn!("Failed to create default admin: {}", e);
        } else {
            info!(
                "Created default admin user: {}",
                config.default_admin_username
            );
        }

        service
    }

    /// Create a new auth service with a custom user store.
    pub fn with_store(config: AuthConfig, store: Arc<dyn UserStore>) -> Self {
        let jwt_service = JwtService::new(&config.jwt_secret, config.jwt_expiration_hours);

        Self {
            jwt_service,
            user_store: store,
            config,
        }
    }

    /// Get a reference to the JWT service.
    pub fn jwt_service(&self) -> &JwtService {
        &self.jwt_service
    }

    /// Check if registration is enabled.
    pub fn is_registration_enabled(&self) -> bool {
        self.config.allow_registration
    }

    // ========================
    // User Management
    // ========================

    /// Register a new user via public registration.
    ///
    /// This method respects the `allow_registration` config setting and
    /// assigns the configured default role to new users.
    ///
    /// Returns an error if registration is disabled or the user already exists.
    pub fn register_user(&mut self, username: &str, password: &str) -> Result<()> {
        if !self.config.allow_registration {
            return Err(AuthError::PermissionDenied(
                "user registration is disabled".to_string(),
            ));
        }

        // Validate username
        if username.is_empty() || username.len() < 3 {
            return Err(AuthError::PermissionDenied(
                "username must be at least 3 characters".to_string(),
            ));
        }

        // Validate password
        if password.len() < 6 {
            return Err(AuthError::PermissionDenied(
                "password must be at least 6 characters".to_string(),
            ));
        }

        // Check if user already exists
        if self.user_store.user_exists(username)? {
            return Err(AuthError::UserAlreadyExists(username.to_string()));
        }

        // Determine role based on config
        let role = match &self.config.default_user_role {
            DefaultUserRole::FullAccess => Role::user_full_access(),
            DefaultUserRole::ReadOnly => Role::ReadOnly {
                allowed_topics: vec!["#".to_string()],
            },
            DefaultUserRole::LimitedAccess(permissions) => Role::User {
                allowed_topics: permissions.clone(),
            },
        };

        let password_hash = hash_password(password)?;
        let user = User::new(username, password_hash, role);
        self.user_store.add_user(user)?;
        info!("Registered new user: {}", username);
        Ok(())
    }

    /// Add a new user with the given credentials and role.
    pub fn add_user(&mut self, username: &str, password: &str, role: Role) -> Result<()> {
        let password_hash = hash_password(password)?;
        let user = User::new(username, password_hash, role);
        self.user_store.add_user(user)?;
        info!("Added new user: {}", username);
        Ok(())
    }

    /// Remove a user by username.
    pub fn remove_user(&self, username: &str) -> Result<bool> {
        let removed = self.user_store.remove_user(username)?;
        if removed {
            info!("Removed user: {}", username);
        }
        Ok(removed)
    }

    /// Get a user by username.
    pub fn get_user(&self, username: &str) -> Result<Option<User>> {
        self.user_store.get_user(username)
    }

    /// Check if a user exists.
    pub fn user_exists(&self, username: &str) -> Result<bool> {
        self.user_store.user_exists(username)
    }

    /// List all usernames.
    pub fn list_users(&self) -> Result<Vec<String>> {
        self.user_store.list_usernames()
    }

    /// Update a user's role.
    pub fn update_user_role(&self, username: &str, role: Role) -> Result<()> {
        let mut user = self
            .user_store
            .get_user(username)?
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        user.role = role;
        self.user_store.update_user(user)?;
        info!("Updated role for user: {}", username);
        Ok(())
    }

    /// Update a user's password.
    pub fn update_user_password(&self, username: &str, new_password: &str) -> Result<()> {
        let mut user = self
            .user_store
            .get_user(username)?
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        user.password_hash = hash_password(new_password)?;
        self.user_store.update_user(user)?;
        info!("Updated password for user: {}", username);
        Ok(())
    }

    // ========================
    // Authentication
    // ========================

    /// Authenticate a user and return a JWT token.
    pub fn login(&self, username: &str, password: &str) -> Result<String> {
        let user = self
            .user_store
            .get_user(username)?
            .ok_or(AuthError::InvalidCredentials)?;

        if !user.enabled {
            warn!("Login attempt for disabled user: {}", username);
            return Err(AuthError::InvalidCredentials);
        }

        if !verify_password(password, &user.password_hash)? {
            warn!("Invalid password for user: {}", username);
            return Err(AuthError::InvalidCredentials);
        }

        let role_name = match &user.role {
            Role::Admin => "admin",
            Role::User { .. } => "user",
            Role::ReadOnly { .. } => "readonly",
        };

        let token = self
            .jwt_service
            .generate_token_with_role(username, role_name)?;

        info!("User logged in: {}", username);
        Ok(token)
    }

    /// Validate a JWT token and return the claims.
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        self.jwt_service.validate_token(token)
    }

    /// Refresh a JWT token.
    pub fn refresh_token(&self, token: &str) -> Result<String> {
        self.jwt_service.refresh_token(token)
    }

    // ========================
    // Authorization
    // ========================

    /// Check if a user can subscribe to a topic.
    pub fn can_subscribe(&self, username: &str, topic: &str) -> bool {
        match self.user_store.get_user(username) {
            Ok(Some(user)) => self.check_subscribe_permission(&user, topic),
            _ => false,
        }
    }

    /// Check if a user can publish to a topic.
    pub fn can_publish(&self, username: &str, topic: &str) -> bool {
        match self.user_store.get_user(username) {
            Ok(Some(user)) => self.check_publish_permission(&user, topic),
            _ => false,
        }
    }

    /// Check subscribe permission for a user object.
    pub fn check_subscribe_permission(&self, user: &User, topic: &str) -> bool {
        if !user.enabled {
            return false;
        }

        match &user.role {
            Role::Admin => true,
            Role::User { allowed_topics } => {
                PermissionChecker::can_subscribe(allowed_topics, topic)
            }
            Role::ReadOnly { allowed_topics } => {
                PermissionChecker::can_subscribe_readonly(allowed_topics, topic)
            }
        }
    }

    /// Check publish permission for a user object.
    pub fn check_publish_permission(&self, user: &User, topic: &str) -> bool {
        if !user.enabled {
            return false;
        }

        match &user.role {
            Role::Admin => true,
            Role::User { allowed_topics } => PermissionChecker::can_publish(allowed_topics, topic),
            Role::ReadOnly { .. } => false, // Read-only users can never publish
        }
    }

    /// Authorize a subscribe action, returning an error if not permitted.
    pub fn authorize_subscribe(&self, username: &str, topic: &str) -> Result<()> {
        if self.can_subscribe(username, topic) {
            Ok(())
        } else {
            Err(AuthError::TopicNotAuthorized {
                topic: topic.to_string(),
                action: "subscribe".to_string(),
            })
        }
    }

    /// Authorize a publish action, returning an error if not permitted.
    pub fn authorize_publish(&self, username: &str, topic: &str) -> Result<()> {
        if self.can_publish(username, topic) {
            Ok(())
        } else {
            Err(AuthError::TopicNotAuthorized {
                topic: topic.to_string(),
                action: "publish".to_string(),
            })
        }
    }
}

/// Builder for creating users with specific permissions.
pub struct UserBuilder {
    username: String,
    password: String,
    permissions: Vec<TopicPermission>,
    is_admin: bool,
    is_readonly: bool,
}

impl UserBuilder {
    /// Start building a new user.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            permissions: Vec::new(),
            is_admin: false,
            is_readonly: false,
        }
    }

    /// Make this user an admin (overrides other permissions).
    pub fn admin(mut self) -> Self {
        self.is_admin = true;
        self
    }

    /// Make this user read-only (can only subscribe, not publish).
    pub fn read_only(mut self) -> Self {
        self.is_readonly = true;
        self
    }

    /// Allow full access (subscribe + publish) to a topic pattern.
    pub fn allow_topic(mut self, pattern: impl Into<String>) -> Self {
        self.permissions.push(TopicPermission::full_access(pattern));
        self
    }

    /// Allow subscribe-only access to a topic pattern.
    pub fn allow_subscribe(mut self, pattern: impl Into<String>) -> Self {
        self.permissions.push(TopicPermission::read_only(pattern));
        self
    }

    /// Allow publish-only access to a topic pattern.
    pub fn allow_publish(mut self, pattern: impl Into<String>) -> Self {
        self.permissions.push(TopicPermission::write_only(pattern));
        self
    }

    /// Add a custom permission.
    pub fn with_permission(mut self, permission: TopicPermission) -> Self {
        self.permissions.push(permission);
        self
    }

    /// Build the role for this user.
    pub fn build_role(self) -> Role {
        if self.is_admin {
            Role::Admin
        } else if self.is_readonly {
            Role::ReadOnly {
                allowed_topics: self.permissions.iter().map(|p| p.pattern.clone()).collect(),
            }
        } else {
            Role::User {
                allowed_topics: self.permissions,
            }
        }
    }

    /// Register this user with an auth service.
    pub fn register(self, auth: &mut AuthService) -> Result<()> {
        let username = self.username.clone();
        let password = self.password.clone();
        let role = self.build_role();
        auth.add_user(&username, &password, role)
    }
}
