//! `AuthService`, its configuration, and the registration rules for usernames and passwords.
//!
//! The service is the crate's entry point: it combines `JwtService`, a
//! `UserStore` and `PermissionChecker` to log users in, manage accounts and
//! answer topic access questions. `UserBuilder` is a shorthand for adding users
//! with a role assembled from topic grants.

use popsub_utils::pii::pii;
use std::sync::Arc;

use tracing::{info, warn};

use crate::error::{AuthError, Result};
use crate::jwt::{Claims, JwtService};
use crate::permission::{PermissionChecker, TopicPermission};
use crate::store::{InMemoryUserStore, UserStore, hash_password, verify_password};
use crate::user::{Role, User};

/// Shortest password, in characters, that `register_user` accepts.
pub const MIN_PASSWORD_LENGTH: usize = 8;

/// Allowed username length, in characters, for `register_user`.
pub const USERNAME_LENGTH: std::ops::RangeInclusive<usize> = 3..=32;

/// Checks a username is 3 to 32 characters of ASCII letters, digits, `_`, `.` or `-`.
///
/// Args:
///     username: The requested username.
///
/// Raises:
///     AuthError::PermissionDenied: When the username breaks either rule.
pub fn validate_username(username: &str) -> Result<()> {
    if !USERNAME_LENGTH.contains(&username.chars().count()) {
        return Err(AuthError::PermissionDenied(format!(
            "username must be {} to {} characters",
            USERNAME_LENGTH.start(),
            USERNAME_LENGTH.end()
        )));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
    if !username.chars().all(allowed) {
        return Err(AuthError::PermissionDenied(
            "username may only contain letters, digits, '_', '.' and '-'".to_string(),
        ));
    }
    Ok(())
}

/// Checks a password has at least `MIN_PASSWORD_LENGTH` characters.
///
/// Args:
///     password: The requested password.
///
/// Raises:
///     AuthError::PermissionDenied: When the password is too short.
pub fn validate_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(AuthError::PermissionDenied(format!(
            "password must be at least {MIN_PASSWORD_LENGTH} characters"
        )));
    }
    Ok(())
}

/// Settings for `AuthService`: JWT secret and lifetime, the bootstrap admin, and registration.
///
/// `Default` uses published values for the secret and admin password, which
/// `insecure_defaults` reports. Registration is off by default and new users
/// get no topics.
#[derive(Clone)]
pub struct AuthConfig {
    /// HMAC secret used to sign and verify tokens.
    pub jwt_secret: String,
    /// Lifetime of issued tokens, in hours.
    pub jwt_expiration_hours: u64,
    /// Username of the admin that `AuthService::new` and
    /// `AuthService::with_store` create when it does not exist.
    pub default_admin_username: String,
    /// Password for that admin.
    pub default_admin_password: String,
    /// Whether `AuthService::register_user` accepts new accounts.
    pub allow_registration: bool,
    /// Role given to accounts created by `AuthService::register_user`.
    pub default_user_role: DefaultUserRole,
}

/// Which role `AuthService::register_user` gives to a self-registered user.
#[derive(Debug, Clone)]
pub enum DefaultUserRole {
    /// Subscribe and publish on every topic, as `Role::user_full_access`.
    FullAccess,
    /// Subscribe to every topic (a `#` pattern), never publish.
    ReadOnly,
    /// A `Role::User` with exactly these permissions. An empty list, the
    /// default, grants nothing.
    LimitedAccess(Vec<TopicPermission>),
}

impl std::fmt::Debug for AuthConfig {
    /// Formats the config with the JWT secret and admin password redacted,
    /// so logging the service never leaks them.
    ///
    /// Args:
    ///     f: The formatter to write to.
    ///
    /// Returns:
    ///     The formatter's result.
    ///
    /// Raises:
    ///     std::fmt::Error: When the formatter fails to write.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("jwt_secret", &"<redacted>")
            .field("jwt_expiration_hours", &self.jwt_expiration_hours)
            .field("default_admin_username", &self.default_admin_username)
            .field("default_admin_password", &"<redacted>")
            .field("allow_registration", &self.allow_registration)
            .field("default_user_role", &self.default_user_role)
            .finish()
    }
}

impl Default for AuthConfig {
    /// Builds a config with published secrets, a 24 hour token lifetime, admin `admin`, and registration off.
    ///
    /// Returns:
    ///     The default config. Only suitable for local use.
    fn default() -> Self {
        Self {
            jwt_secret: "change_me_in_production".to_string(),
            jwt_expiration_hours: 24,
            default_admin_username: "admin".to_string(),
            default_admin_password: "password".to_string(),
            allow_registration: false,
            default_user_role: DefaultUserRole::LimitedAccess(Vec::new()),
        }
    }
}

/// JWT secrets that appear in PopSub's code or sample config.
const PUBLISHED_SECRETS: [&str; 3] = [
    "change_me_in_production",
    "default_secret",
    "a_very_secret_key",
];

/// The admin password that appears in PopSub's code and sample config.
const PUBLISHED_PASSWORD: &str = "password";

impl AuthConfig {
    /// Lists settings that still use values published with PopSub.
    ///
    /// Returns:
    ///     A description of each insecure setting, empty when there are none.
    pub fn insecure_defaults(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if PUBLISHED_SECRETS.contains(&self.jwt_secret.as_str()) {
            found.push("the JWT secret is a published default");
        }
        if self.default_admin_password == PUBLISHED_PASSWORD {
            found.push("the default admin password is the published default");
        }
        found
    }

    /// Logs a warning for each setting `insecure_defaults` reports.
    fn warn_if_insecure(&self) {
        for problem in self.insecure_defaults() {
            warn!("{problem}; set your own before exposing this service");
        }
    }

    /// Builds the default config with a caller-supplied JWT secret.
    ///
    /// Args:
    ///     jwt_secret: The HMAC secret for signing tokens.
    ///
    /// Returns:
    ///     The config, with every other field at its default.
    pub fn new(jwt_secret: impl Into<String>) -> Self {
        Self {
            jwt_secret: jwt_secret.into(),
            ..Default::default()
        }
    }

    /// Sets the token lifetime.
    ///
    /// Args:
    ///     hours: Hours until issued tokens expire.
    ///
    /// Returns:
    ///     The updated config.
    pub fn with_expiration(mut self, hours: u64) -> Self {
        self.jwt_expiration_hours = hours;
        self
    }

    /// Sets the username and password of the bootstrap admin.
    ///
    /// Args:
    ///     username: The admin's username.
    ///     password: The admin's plain text password, hashed when the admin is created.
    ///
    /// Returns:
    ///     The updated config.
    pub fn with_admin(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.default_admin_username = username.into();
        self.default_admin_password = password.into();
        self
    }

    /// Turns self-registration through `register_user` on or off.
    ///
    /// Args:
    ///     enabled: Whether registration is allowed.
    ///
    /// Returns:
    ///     The updated config.
    pub fn with_registration(mut self, enabled: bool) -> Self {
        self.allow_registration = enabled;
        self
    }

    /// Sets the role given to self-registered users.
    ///
    /// Args:
    ///     role: The role to assign at registration.
    ///
    /// Returns:
    ///     The updated config.
    pub fn with_default_role(mut self, role: DefaultUserRole) -> Self {
        self.default_user_role = role;
        self
    }
}

/// Logs users in, issues and checks tokens, manages accounts, and decides topic access.
///
/// Its `Debug` output includes the config (and so the JWT secret and admin
/// password) but not the store.
pub struct AuthService {
    jwt_service: JwtService,
    /// The store holding every account. Public so tests and callers can edit
    /// users directly, for example to disable one.
    pub user_store: Arc<dyn UserStore>,
    config: AuthConfig,
}

impl std::fmt::Debug for AuthService {
    /// Writes the JWT service and config, leaving out the user store.
    ///
    /// Args:
    ///     f: The formatter to write to.
    ///
    /// Returns:
    ///     The formatter's result.
    ///
    /// Raises:
    ///     std::fmt::Error: When the formatter fails to write.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthService")
            .field("jwt_service", &self.jwt_service)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AuthService {
    /// Builds a service on a fresh in-memory store and adds the configured admin.
    ///
    /// Args:
    ///     config: Secret, token lifetime, admin credentials and registration settings.
    ///
    /// Returns:
    ///     The service, holding only the admin account.
    pub async fn new(config: AuthConfig) -> Self {
        Self::with_store(config, Arc::new(InMemoryUserStore::new())).await
    }

    /// Builds a service on an existing store and adds the configured admin if it is missing.
    ///
    /// Logs a warning for each published default in `config`. An existing
    /// account with the admin's username is left as it is, even if its role or
    /// password differ from the config. A failure to create the admin is
    /// logged and the service is still returned.
    ///
    /// Args:
    ///     config: Secret, token lifetime, admin credentials and registration settings.
    ///     store: The user store to read and write accounts in.
    ///
    /// Returns:
    ///     The service.
    pub async fn with_store(config: AuthConfig, store: Arc<dyn UserStore>) -> Self {
        config.warn_if_insecure();
        let service = Self {
            jwt_service: JwtService::new(&config.jwt_secret, config.jwt_expiration_hours),
            user_store: store,
            config,
        };
        service.ensure_admin().await;
        service
    }

    /// Creates the configured admin account unless one with that username exists.
    ///
    /// If the existence check fails, creation is attempted anyway. Failures
    /// are logged rather than returned, so a store problem doesn't stop the
    /// service from starting.
    async fn ensure_admin(&self) {
        let username = &self.config.default_admin_username;
        if self.user_exists(username).await.unwrap_or(false) {
            return;
        }
        match self
            .add_user(username, &self.config.default_admin_password, Role::Admin)
            .await
        {
            Ok(()) => info!(username = %pii(username), "created the default admin user"),
            Err(e) => {
                warn!(username = %pii(username), error = e.kind(), "failed to create the default admin user")
            }
        }
    }

    /// Exposes the JWT service so callers can sign or decode tokens directly.
    ///
    /// Returns:
    ///     The service's `JwtService`.
    pub fn jwt_service(&self) -> &JwtService {
        &self.jwt_service
    }

    /// Reports whether `register_user` accepts new accounts.
    ///
    /// Returns:
    ///     The config's `allow_registration` flag.
    pub fn is_registration_enabled(&self) -> bool {
        self.config.allow_registration
    }

    /// Creates an account through self-registration, with the configured default role.
    ///
    /// Unlike `add_user`, this enforces the registration switch and the
    /// username and password rules. The existence check and the insert are not
    /// atomic, but the store still refuses a duplicate on insert.
    ///
    /// Args:
    ///     username: The requested username.
    ///     password: The plain text password, hashed before it is stored.
    ///
    /// Raises:
    ///     AuthError::PermissionDenied: When registration is disabled, or the
    ///         username or password breaks the registration rules.
    ///     AuthError::UserAlreadyExists: When the username is taken.
    ///     AuthError::PasswordHashError: When hashing the password fails.
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn register_user(&self, username: &str, password: &str) -> Result<()> {
        if !self.config.allow_registration {
            return Err(AuthError::PermissionDenied(
                "user registration is disabled".to_string(),
            ));
        }

        validate_username(username)?;
        validate_password(password)?;

        if self.user_store.user_exists(username).await? {
            return Err(AuthError::UserAlreadyExists(username.to_string()));
        }

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
        self.user_store.add_user(user).await?;
        info!(username = %pii(username), "registered a new user");
        Ok(())
    }

    /// Creates an account with any role, skipping the registration switch and rules.
    ///
    /// This is the admin path: usernames and passwords are not validated.
    ///
    /// Args:
    ///     username: The new account's username.
    ///     password: The plain text password, hashed before it is stored.
    ///     role: The account's role.
    ///
    /// Raises:
    ///     AuthError::PasswordHashError: When hashing the password fails.
    ///     AuthError::UserAlreadyExists: When the username is taken.
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn add_user(&self, username: &str, password: &str, role: Role) -> Result<()> {
        let password_hash = hash_password(password)?;
        let user = User::new(username, password_hash, role);
        self.user_store.add_user(user).await?;
        info!(username = %pii(username), "added a user");
        Ok(())
    }

    /// Deletes an account. Tokens already issued to it stay valid until they expire.
    ///
    /// Args:
    ///     username: The account to delete.
    ///
    /// Returns:
    ///     `true` if an account was removed, `false` if none existed.
    ///
    /// Raises:
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn remove_user(&self, username: &str) -> Result<bool> {
        let removed = self.user_store.remove_user(username).await?;
        if removed {
            info!(username = %pii(username), "removed a user");
        }
        Ok(removed)
    }

    /// Looks up an account in the store.
    ///
    /// Args:
    ///     username: The account to fetch.
    ///
    /// Returns:
    ///     The user, or `None` if there is no such account.
    ///
    /// Raises:
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn get_user(&self, username: &str) -> Result<Option<User>> {
        self.user_store.get_user(username).await
    }

    /// Reports whether an account with this username is in the store.
    ///
    /// Args:
    ///     username: The username to look for.
    ///
    /// Returns:
    ///     `true` if the account exists.
    ///
    /// Raises:
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn user_exists(&self, username: &str) -> Result<bool> {
        self.user_store.user_exists(username).await
    }

    /// Lists the username of every account in the store.
    ///
    /// Returns:
    ///     All usernames. The in-memory store returns them in no set order;
    ///     the PostgreSQL store sorts them.
    ///
    /// Raises:
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn list_users(&self) -> Result<Vec<String>> {
        self.user_store.list_usernames().await
    }

    /// Replaces an account's role. Takes effect on the user's next permission check.
    ///
    /// Args:
    ///     username: The account to change.
    ///     role: The new role.
    ///
    /// Raises:
    ///     AuthError::UserNotFound: When there is no such account.
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn update_user_role(&self, username: &str, role: Role) -> Result<()> {
        let mut user = self
            .user_store
            .get_user(username)
            .await?
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        user.role = role;
        self.user_store.update_user(user).await?;
        info!(username = %pii(username), "changed a user's role");
        Ok(())
    }

    /// Enables or disables an account. A disabled account can't log in and is
    /// refused every topic, effective on its next permission check.
    ///
    /// Args:
    ///     username: The account.
    ///     enabled: Whether it may log in and use topics.
    ///
    /// Raises:
    ///     AuthError::UserNotFound: When there is no such account.
    ///     AuthError::Internal: When the user store fails.
    pub async fn set_user_enabled(&self, username: &str, enabled: bool) -> Result<()> {
        let mut user = self
            .user_store
            .get_user(username)
            .await?
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;
        user.enabled = enabled;
        self.user_store.update_user(user).await?;
        info!(username = %pii(username), enabled, "changed whether a user is enabled");
        Ok(())
    }

    /// Counts the accounts that are enabled admins, so the last one can't be
    /// disabled, deleted or demoted.
    ///
    /// Returns:
    ///     The count.
    ///
    /// Raises:
    ///     AuthError::Internal: When the user store fails.
    pub async fn enabled_admin_count(&self) -> Result<usize> {
        let mut count = 0;
        for username in self.user_store.list_usernames().await? {
            if let Some(user) = self.user_store.get_user(&username).await?
                && user.enabled
                && user.is_admin()
            {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Replaces an account's password, without applying the registration length rule.
    ///
    /// Existing tokens are not revoked.
    ///
    /// Args:
    ///     username: The account to change.
    ///     new_password: The new plain text password, hashed before it is stored.
    ///
    /// Raises:
    ///     AuthError::UserNotFound: When there is no such account.
    ///     AuthError::PasswordHashError: When hashing the password fails.
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn update_user_password(&self, username: &str, new_password: &str) -> Result<()> {
        let mut user = self
            .user_store
            .get_user(username)
            .await?
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        user.password_hash = hash_password(new_password)?;
        user.token_version = user.token_version.saturating_add(1);
        self.user_store.update_user(user).await?;
        info!(username = %pii(username), "changed a user's password");
        Ok(())
    }

    /// Checks a username and password and issues a token carrying the user's role name.
    ///
    /// Unknown users, disabled users and wrong passwords all return the same
    /// error, so callers cannot tell which one failed.
    ///
    /// Args:
    ///     username: The account to log in as.
    ///     password: The plain text password to check.
    ///
    /// Returns:
    ///     A signed token valid for the configured lifetime.
    ///
    /// Raises:
    ///     AuthError::InvalidCredentials: When the user is unknown, disabled,
    ///         or the password is wrong.
    ///     AuthError::PasswordHashError: When the stored hash cannot be parsed.
    ///     AuthError::JwtError: When signing the token fails.
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn login(&self, username: &str, password: &str) -> Result<String> {
        let user = self
            .user_store
            .get_user(username)
            .await?
            .ok_or(AuthError::InvalidCredentials)?;

        if !user.enabled {
            warn!(username = %pii(username), "login attempt for a disabled user");
            return Err(AuthError::InvalidCredentials);
        }

        if !verify_password(password, &user.password_hash)? {
            warn!(username = %pii(username), "wrong password");
            return Err(AuthError::InvalidCredentials);
        }

        let role_name = match &user.role {
            Role::Admin => "admin",
            Role::User { .. } => "user",
            Role::ReadOnly { .. } => "readonly",
        };

        let token =
            self.jwt_service
                .generate_token_versioned(username, role_name, user.token_version)?;

        info!(username = %pii(username), "logged in");
        Ok(token)
    }

    /// Verifies a token's signature and expiry and decodes its claims.
    ///
    /// Only the token is checked. The user may since have been removed or disabled.
    ///
    /// Args:
    ///     token: The encoded token.
    ///
    /// Returns:
    ///     The decoded claims.
    ///
    /// Raises:
    ///     AuthError::TokenExpired: When the token has expired.
    ///     AuthError::JwtError: When the token is malformed or badly signed.
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        self.jwt_service.validate_token(token)
    }

    /// Issues a new token with a full lifetime for the holder of a still valid one.
    ///
    /// The user is looked up first, so an account that has been removed or
    /// disabled since the token was issued can't keep itself alive by
    /// refreshing.
    ///
    /// Args:
    ///     token: The token to refresh.
    ///
    /// Returns:
    ///     The new token, with the same subject and role claim.
    ///
    /// Raises:
    ///     AuthError::TokenExpired: When the token has expired.
    ///     AuthError::JwtError: When the token is malformed or badly signed,
    ///         or the new token can't be signed.
    ///     AuthError::InvalidToken: When the token's user no longer exists, is
    ///         disabled, or changed its password after the token was issued.
    pub async fn refresh_token(&self, token: &str) -> Result<String> {
        let claims = self.jwt_service.validate_token(token)?;
        match self.user_store.get_user(&claims.sub).await? {
            Some(user) if user.enabled && user.token_version == claims.ver => {
                self.jwt_service.refresh_token(token)
            }
            _ => Err(AuthError::InvalidToken),
        }
    }

    /// Decides whether a user may subscribe to a topic, using the role in the store.
    ///
    /// Unknown users and store errors both count as a refusal.
    ///
    /// Args:
    ///     username: The account to check.
    ///     topic: The topic to subscribe to.
    ///
    /// Returns:
    ///     `true` when the user exists, is enabled and its role allows subscribing.
    pub async fn can_subscribe(&self, username: &str, topic: &str) -> bool {
        match self.user_store.get_user(username).await {
            Ok(Some(user)) => self.check_subscribe_permission(&user, topic),
            _ => false,
        }
    }

    /// Decides whether a user may publish to a topic, using the role in the store.
    ///
    /// Unknown users and store errors both count as a refusal.
    ///
    /// Args:
    ///     username: The account to check.
    ///     topic: The topic to publish to.
    ///
    /// Returns:
    ///     `true` when the user exists, is enabled and its role allows publishing.
    pub async fn can_publish(&self, username: &str, topic: &str) -> bool {
        match self.user_store.get_user(username).await {
            Ok(Some(user)) => self.check_publish_permission(&user, topic),
            _ => false,
        }
    }

    /// Decides whether an already loaded user may subscribe to a topic.
    ///
    /// Disabled users are refused. Admins may subscribe anywhere, `Role::User`
    /// needs a matching permission with `can_subscribe`, and `Role::ReadOnly`
    /// needs a matching pattern.
    ///
    /// Args:
    ///     user: The user to check.
    ///     topic: The topic to subscribe to.
    ///
    /// Returns:
    ///     `true` when the subscription is allowed.
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

    /// Decides whether an already loaded user may publish to a topic.
    ///
    /// Disabled users are refused. Admins may publish anywhere, `Role::User`
    /// needs a matching permission with `can_publish`, and `Role::ReadOnly`
    /// users can never publish.
    ///
    /// Args:
    ///     user: The user to check.
    ///     topic: The topic to publish to.
    ///
    /// Returns:
    ///     `true` when publishing is allowed.
    pub fn check_publish_permission(&self, user: &User, topic: &str) -> bool {
        if !user.enabled {
            return false;
        }

        match &user.role {
            Role::Admin => true,
            Role::User { allowed_topics } => PermissionChecker::can_publish(allowed_topics, topic),
            Role::ReadOnly { .. } => false,
        }
    }

    /// Same check as `can_subscribe`, but reports a refusal as an error.
    ///
    /// Args:
    ///     username: The account to check.
    ///     topic: The topic to subscribe to.
    ///
    /// Raises:
    ///     AuthError::TopicNotAuthorized: When the user is unknown, disabled,
    ///         not allowed to subscribe, or the store lookup fails. `action`
    ///         is `"subscribe"`.
    pub async fn authorize_subscribe(&self, username: &str, topic: &str) -> Result<()> {
        if self.can_subscribe(username, topic).await {
            Ok(())
        } else {
            Err(AuthError::TopicNotAuthorized {
                topic: topic.to_string(),
                action: "subscribe".to_string(),
            })
        }
    }

    /// Same check as `can_publish`, but reports a refusal as an error.
    ///
    /// Args:
    ///     username: The account to check.
    ///     topic: The topic to publish to.
    ///
    /// Raises:
    ///     AuthError::TopicNotAuthorized: When the user is unknown, disabled,
    ///         not allowed to publish, or the store lookup fails. `action` is
    ///         `"publish"`.
    pub async fn authorize_publish(&self, username: &str, topic: &str) -> Result<()> {
        if self.can_publish(username, topic).await {
            Ok(())
        } else {
            Err(AuthError::TopicNotAuthorized {
                topic: topic.to_string(),
                action: "publish".to_string(),
            })
        }
    }
}

/// Collects a username, password and topic grants, then adds the user through `AuthService::add_user`.
///
/// `admin` wins over `read_only`, which wins over the plain topic grants.
pub struct UserBuilder {
    username: String,
    password: String,
    permissions: Vec<TopicPermission>,
    is_admin: bool,
    is_readonly: bool,
}

impl UserBuilder {
    /// Starts a builder for a non-admin user with no topic grants.
    ///
    /// Args:
    ///     username: The new account's username.
    ///     password: The plain text password.
    ///
    /// Returns:
    ///     The builder.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            permissions: Vec::new(),
            is_admin: false,
            is_readonly: false,
        }
    }

    /// Marks the user as an admin, so every topic grant is ignored.
    ///
    /// Returns:
    ///     The updated builder.
    pub fn admin(mut self) -> Self {
        self.is_admin = true;
        self
    }

    /// Marks the user as read-only, so it may subscribe to every granted pattern and never publish.
    ///
    /// Every grant's pattern becomes subscribable, including patterns added with
    /// `allow_publish`.
    ///
    /// Returns:
    ///     The updated builder.
    pub fn read_only(mut self) -> Self {
        self.is_readonly = true;
        self
    }

    /// Grants subscribe and publish on a topic pattern.
    ///
    /// Args:
    ///     pattern: The topic pattern to grant.
    ///
    /// Returns:
    ///     The updated builder.
    pub fn allow_topic(mut self, pattern: impl Into<String>) -> Self {
        self.permissions.push(TopicPermission::full_access(pattern));
        self
    }

    /// Grants subscribe only on a topic pattern.
    ///
    /// Args:
    ///     pattern: The topic pattern to grant.
    ///
    /// Returns:
    ///     The updated builder.
    pub fn allow_subscribe(mut self, pattern: impl Into<String>) -> Self {
        self.permissions.push(TopicPermission::read_only(pattern));
        self
    }

    /// Grants publish only on a topic pattern.
    ///
    /// Args:
    ///     pattern: The topic pattern to grant.
    ///
    /// Returns:
    ///     The updated builder.
    pub fn allow_publish(mut self, pattern: impl Into<String>) -> Self {
        self.permissions.push(TopicPermission::write_only(pattern));
        self
    }

    /// Adds a permission with caller-chosen flags.
    ///
    /// Args:
    ///     permission: The permission to grant.
    ///
    /// Returns:
    ///     The updated builder.
    pub fn with_permission(mut self, permission: TopicPermission) -> Self {
        self.permissions.push(permission);
        self
    }

    /// Turns the collected flags and grants into a role.
    ///
    /// Returns:
    ///     `Role::Admin` if `admin` was called, otherwise `Role::ReadOnly` with
    ///     every granted pattern if `read_only` was called, otherwise
    ///     `Role::User` with the grants.
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

    /// Adds the user to an auth service with the built role, through `add_user`.
    ///
    /// The registration switch and the username and password rules do not apply.
    ///
    /// Args:
    ///     auth: The service to add the user to.
    ///
    /// Raises:
    ///     AuthError::PasswordHashError: When hashing the password fails.
    ///     AuthError::UserAlreadyExists: When the username is taken.
    ///     AuthError::Internal: When the user store fails, such as a poisoned
    ///         lock or a database error.
    pub async fn register(self, auth: &AuthService) -> Result<()> {
        let username = self.username.clone();
        let password = self.password.clone();
        let role = self.build_role();
        auth.add_user(&username, &password, role).await
    }
}
