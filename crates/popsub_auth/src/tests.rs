//! Integration tests for popsub_auth
//!
//! Tests the full authentication and authorization flow.

use crate::permission::TopicPermission;
use crate::service::{AuthConfig, AuthService, UserBuilder};
use crate::user::Role;

#[test]
fn test_full_auth_flow() {
    let config = AuthConfig::new("test_secret").with_admin("admin", "admin123");

    let auth = AuthService::new(config);

    // Login as admin
    let token = auth.login("admin", "admin123").unwrap();
    assert!(!token.is_empty());

    // Validate token
    let claims = auth.validate_token(&token).unwrap();
    assert_eq!(claims.sub, "admin");
    assert_eq!(claims.role, Some("admin".to_string()));

    // Admin can publish/subscribe to any topic
    assert!(auth.can_subscribe("admin", "any/topic"));
    assert!(auth.can_publish("admin", "any/topic"));
}

#[test]
fn test_invalid_login() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config);

    // Wrong password
    let result = auth.login("admin", "wrong_password");
    assert!(result.is_err());

    // Non-existent user
    let result = auth.login("nonexistent", "password");
    assert!(result.is_err());
}

#[test]
fn test_user_with_limited_permissions() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    // Add a user with limited permissions
    let role = Role::User {
        allowed_topics: vec![
            TopicPermission::full_access("chat/#"),
            TopicPermission::read_only("sensors/+/temperature"),
        ],
    };
    auth.add_user("alice", "alice123", role).unwrap();

    // Login
    let token = auth.login("alice", "alice123").unwrap();
    let claims = auth.validate_token(&token).unwrap();
    assert_eq!(claims.sub, "alice");

    // Check permissions
    assert!(auth.can_subscribe("alice", "chat/room1"));
    assert!(auth.can_publish("alice", "chat/room1"));
    assert!(auth.can_subscribe("alice", "chat/room1/messages"));
    assert!(auth.can_publish("alice", "chat/room1/messages"));

    assert!(auth.can_subscribe("alice", "sensors/living_room/temperature"));
    assert!(!auth.can_publish("alice", "sensors/living_room/temperature")); // read-only

    assert!(!auth.can_subscribe("alice", "other/topic"));
    assert!(!auth.can_publish("alice", "other/topic"));
}

#[test]
fn test_readonly_user() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    let role = Role::ReadOnly {
        allowed_topics: vec!["news/#".to_string(), "updates/*".to_string()],
    };
    auth.add_user("reader", "reader123", role).unwrap();

    assert!(auth.can_subscribe("reader", "news/tech"));
    assert!(auth.can_subscribe("reader", "updates/v1"));
    assert!(!auth.can_publish("reader", "news/tech")); // readonly can never publish
    assert!(!auth.can_publish("reader", "updates/v1"));
}

#[test]
fn test_user_builder() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    UserBuilder::new("bob", "bob123")
        .allow_topic("chat/#")
        .allow_subscribe("sensors/*")
        .register(&mut auth)
        .unwrap();

    assert!(auth.can_subscribe("bob", "chat/room1"));
    assert!(auth.can_publish("bob", "chat/room1"));
    assert!(auth.can_subscribe("bob", "sensors/temp"));
    assert!(!auth.can_publish("bob", "sensors/temp"));
}

#[test]
fn test_user_builder_admin() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    UserBuilder::new("superadmin", "super123")
        .admin()
        .register(&mut auth)
        .unwrap();

    assert!(auth.can_subscribe("superadmin", "anything"));
    assert!(auth.can_publish("superadmin", "anything"));
}

#[test]
fn test_token_refresh() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config);

    let token = auth.login("admin", "password").unwrap();
    let new_token = auth.refresh_token(&token).unwrap();

    let claims = auth.validate_token(&new_token).unwrap();
    assert_eq!(claims.sub, "admin");
}

#[test]
fn test_authorization_errors() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    let role = Role::User {
        allowed_topics: vec![TopicPermission::full_access("allowed/#")],
    };
    auth.add_user("limited", "limited123", role).unwrap();

    // Should succeed
    assert!(auth.authorize_subscribe("limited", "allowed/topic").is_ok());
    assert!(auth.authorize_publish("limited", "allowed/topic").is_ok());

    // Should fail
    assert!(auth.authorize_subscribe("limited", "denied/topic").is_err());
    assert!(auth.authorize_publish("limited", "denied/topic").is_err());
}

#[test]
fn test_user_management() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    // Add user
    auth.add_user("newuser", "password123", Role::default())
        .unwrap();
    assert!(auth.user_exists("newuser").unwrap());

    // List users
    let users = auth.list_users().unwrap();
    assert!(users.contains(&"newuser".to_string()));
    assert!(users.contains(&"admin".to_string()));

    // Update password
    auth.update_user_password("newuser", "newpassword").unwrap();
    assert!(auth.login("newuser", "newpassword").is_ok());
    assert!(auth.login("newuser", "password123").is_err());

    // Update role
    auth.update_user_role("newuser", Role::Admin).unwrap();
    let user = auth.get_user("newuser").unwrap().unwrap();
    assert!(user.is_admin());

    // Remove user
    auth.remove_user("newuser").unwrap();
    assert!(!auth.user_exists("newuser").unwrap());
}

#[test]
fn test_disabled_user() {
    let config = AuthConfig::default();
    let mut auth = AuthService::new(config);

    auth.add_user("tobedisabled", "password", Role::Admin)
        .unwrap();

    // Manually disable the user
    let mut user = auth.get_user("tobedisabled").unwrap().unwrap();
    user.enabled = false;
    auth.user_store
        .as_ref()
        .update_user(user)
        .expect("update failed");

    // Login should fail for disabled user
    assert!(auth.login("tobedisabled", "password").is_err());

    // Permissions should be denied
    assert!(!auth.can_subscribe("tobedisabled", "any/topic"));
    assert!(!auth.can_publish("tobedisabled", "any/topic"));
}

#[test]
fn test_user_registration() {
    let config = AuthConfig::default().with_registration(true);
    let mut auth = AuthService::new(config);

    // Register a new user
    auth.register_user("newuser", "password123").unwrap();

    // User should exist
    assert!(auth.user_exists("newuser").unwrap());

    // User should be able to login
    let token = auth.login("newuser", "password123").unwrap();
    assert!(!token.is_empty());

    // Default role should be full access
    assert!(auth.can_subscribe("newuser", "any/topic"));
    assert!(auth.can_publish("newuser", "any/topic"));
}

#[test]
fn test_registration_disabled() {
    let config = AuthConfig::default().with_registration(false);
    let mut auth = AuthService::new(config);

    // Registration should fail
    let result = auth.register_user("newuser", "password123");
    assert!(result.is_err());
    assert!(!auth.user_exists("newuser").unwrap());
}

#[test]
fn test_registration_validation() {
    let config = AuthConfig::default().with_registration(true);
    let mut auth = AuthService::new(config);

    // Username too short
    let result = auth.register_user("ab", "password123");
    assert!(result.is_err());

    // Password too short
    let result = auth.register_user("validuser", "12345");
    assert!(result.is_err());

    // Duplicate user
    auth.register_user("uniqueuser", "password123").unwrap();
    let result = auth.register_user("uniqueuser", "different");
    assert!(result.is_err());
}

#[test]
fn test_registration_with_readonly_role() {
    use crate::service::DefaultUserRole;

    let config = AuthConfig::default()
        .with_registration(true)
        .with_default_role(DefaultUserRole::ReadOnly);
    let mut auth = AuthService::new(config);

    auth.register_user("reader", "password123").unwrap();

    // Should be able to subscribe
    assert!(auth.can_subscribe("reader", "any/topic"));
    // Should NOT be able to publish (read-only)
    assert!(!auth.can_publish("reader", "any/topic"));
}

#[test]
fn test_registration_with_limited_role() {
    use crate::service::DefaultUserRole;

    let config = AuthConfig::default()
        .with_registration(true)
        .with_default_role(DefaultUserRole::LimitedAccess(vec![
            TopicPermission::full_access("public/#"),
            TopicPermission::read_only("announcements/*"),
        ]));
    let mut auth = AuthService::new(config);

    auth.register_user("limited", "password123").unwrap();

    // Should have access to public topics
    assert!(auth.can_subscribe("limited", "public/chat"));
    assert!(auth.can_publish("limited", "public/chat"));

    // Should have read-only access to announcements
    assert!(auth.can_subscribe("limited", "announcements/news"));
    assert!(!auth.can_publish("limited", "announcements/news"));

    // Should NOT have access to other topics
    assert!(!auth.can_subscribe("limited", "private/secret"));
    assert!(!auth.can_publish("limited", "private/secret"));
}
