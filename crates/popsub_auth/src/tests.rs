//! Integration tests for popsub_auth
//!
//! Tests the full authentication and authorization flow.

use crate::permission::TopicPermission;
use crate::service::{AuthConfig, AuthService, UserBuilder};
use crate::user::Role;

#[tokio::test]
async fn test_full_auth_flow() {
    let config = AuthConfig::new("test_secret").with_admin("admin", "admin123");

    let auth = AuthService::new(config).await;

    // Login as admin
    let token = auth.login("admin", "admin123").await.unwrap();
    assert!(!token.is_empty());

    // Validate token
    let claims = auth.validate_token(&token).unwrap();
    assert_eq!(claims.sub, "admin");
    assert_eq!(claims.role, Some("admin".to_string()));

    // Admin can publish/subscribe to any topic
    assert!(auth.can_subscribe("admin", "any/topic").await);
    assert!(auth.can_publish("admin", "any/topic").await);
}

#[tokio::test]
async fn test_invalid_login() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    // Wrong password
    let result = auth.login("admin", "wrong_password").await;
    assert!(result.is_err());

    // Non-existent user
    let result = auth.login("nonexistent", "password").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_user_with_limited_permissions() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    // Add a user with limited permissions
    let role = Role::User {
        allowed_topics: vec![
            TopicPermission::full_access("chat/#"),
            TopicPermission::read_only("sensors/+/temperature"),
        ],
    };
    auth.add_user("alice", "alice123", role).await.unwrap();

    // Login
    let token = auth.login("alice", "alice123").await.unwrap();
    let claims = auth.validate_token(&token).unwrap();
    assert_eq!(claims.sub, "alice");

    // Check permissions
    assert!(auth.can_subscribe("alice", "chat/room1").await);
    assert!(auth.can_publish("alice", "chat/room1").await);
    assert!(auth.can_subscribe("alice", "chat/room1/messages").await);
    assert!(auth.can_publish("alice", "chat/room1/messages").await);

    assert!(
        auth.can_subscribe("alice", "sensors/living_room/temperature")
            .await
    );
    assert!(
        !auth
            .can_publish("alice", "sensors/living_room/temperature")
            .await
    ); // read-only

    assert!(!auth.can_subscribe("alice", "other/topic").await);
    assert!(!auth.can_publish("alice", "other/topic").await);
}

#[tokio::test]
async fn test_readonly_user() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let role = Role::ReadOnly {
        allowed_topics: vec!["news/#".to_string(), "updates/*".to_string()],
    };
    auth.add_user("reader", "reader123", role).await.unwrap();

    assert!(auth.can_subscribe("reader", "news/tech").await);
    assert!(auth.can_subscribe("reader", "updates/v1").await);
    assert!(!auth.can_publish("reader", "news/tech").await); // readonly can never publish
    assert!(!auth.can_publish("reader", "updates/v1").await);
}

#[tokio::test]
async fn test_user_builder() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    UserBuilder::new("bob", "bob123")
        .allow_topic("chat/#")
        .allow_subscribe("sensors/*")
        .register(&auth)
        .await
        .unwrap();

    assert!(auth.can_subscribe("bob", "chat/room1").await);
    assert!(auth.can_publish("bob", "chat/room1").await);
    assert!(auth.can_subscribe("bob", "sensors/temp").await);
    assert!(!auth.can_publish("bob", "sensors/temp").await);
}

#[tokio::test]
async fn test_user_builder_admin() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    UserBuilder::new("superadmin", "super123")
        .admin()
        .register(&auth)
        .await
        .unwrap();

    assert!(auth.can_subscribe("superadmin", "anything").await);
    assert!(auth.can_publish("superadmin", "anything").await);
}

#[tokio::test]
async fn test_token_refresh() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let token = auth.login("admin", "password").await.unwrap();
    let new_token = auth.refresh_token(&token).unwrap();

    let claims = auth.validate_token(&new_token).unwrap();
    assert_eq!(claims.sub, "admin");
}

#[tokio::test]
async fn test_authorization_errors() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let role = Role::User {
        allowed_topics: vec![TopicPermission::full_access("allowed/#")],
    };
    auth.add_user("limited", "limited123", role).await.unwrap();

    // Should succeed
    assert!(
        auth.authorize_subscribe("limited", "allowed/topic")
            .await
            .is_ok()
    );
    assert!(
        auth.authorize_publish("limited", "allowed/topic")
            .await
            .is_ok()
    );

    // Should fail
    assert!(
        auth.authorize_subscribe("limited", "denied/topic")
            .await
            .is_err()
    );
    assert!(
        auth.authorize_publish("limited", "denied/topic")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn test_user_management() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    // Add user
    auth.add_user("newuser", "password123", Role::default())
        .await
        .unwrap();
    assert!(auth.user_exists("newuser").await.unwrap());

    // List users
    let users = auth.list_users().await.unwrap();
    assert!(users.contains(&"newuser".to_string()));
    assert!(users.contains(&"admin".to_string()));

    // Update password
    auth.update_user_password("newuser", "newpassword")
        .await
        .unwrap();
    assert!(auth.login("newuser", "newpassword").await.is_ok());
    assert!(auth.login("newuser", "password123").await.is_err());

    // Update role
    auth.update_user_role("newuser", Role::Admin).await.unwrap();
    let user = auth.get_user("newuser").await.unwrap().unwrap();
    assert!(user.is_admin());

    // Remove user
    auth.remove_user("newuser").await.unwrap();
    assert!(!auth.user_exists("newuser").await.unwrap());
}

#[tokio::test]
async fn test_disabled_user() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    auth.add_user("tobedisabled", "password", Role::Admin)
        .await
        .unwrap();

    // Manually disable the user
    let mut user = auth.get_user("tobedisabled").await.unwrap().unwrap();
    user.enabled = false;
    auth.user_store
        .update_user(user)
        .await
        .expect("update failed");

    // Login should fail for disabled user
    assert!(auth.login("tobedisabled", "password").await.is_err());

    // Permissions should be denied
    assert!(!auth.can_subscribe("tobedisabled", "any/topic").await);
    assert!(!auth.can_publish("tobedisabled", "any/topic").await);
}

#[tokio::test]
async fn test_user_registration() {
    let config = AuthConfig::default().with_registration(true);
    let auth = AuthService::new(config).await;

    // Register a new user
    auth.register_user("newuser", "password123").await.unwrap();

    // User should exist
    assert!(auth.user_exists("newuser").await.unwrap());

    // User should be able to login
    let token = auth.login("newuser", "password123").await.unwrap();
    assert!(!token.is_empty());

    // Default role should be full access
    assert!(auth.can_subscribe("newuser", "any/topic").await);
    assert!(auth.can_publish("newuser", "any/topic").await);
}

#[tokio::test]
async fn test_registration_disabled() {
    let config = AuthConfig::default().with_registration(false);
    let auth = AuthService::new(config).await;

    // Registration should fail
    let result = auth.register_user("newuser", "password123").await;
    assert!(result.is_err());
    assert!(!auth.user_exists("newuser").await.unwrap());
}

#[tokio::test]
async fn test_registration_validation() {
    let config = AuthConfig::default().with_registration(true);
    let auth = AuthService::new(config).await;

    // Username too short
    let result = auth.register_user("ab", "password123").await;
    assert!(result.is_err());

    // Password too short
    let result = auth.register_user("validuser", "12345").await;
    assert!(result.is_err());

    // Duplicate user
    auth.register_user("uniqueuser", "password123")
        .await
        .unwrap();
    let result = auth.register_user("uniqueuser", "different").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_registration_with_readonly_role() {
    use crate::service::DefaultUserRole;

    let config = AuthConfig::default()
        .with_registration(true)
        .with_default_role(DefaultUserRole::ReadOnly);
    let auth = AuthService::new(config).await;

    auth.register_user("reader", "password123").await.unwrap();

    // Should be able to subscribe
    assert!(auth.can_subscribe("reader", "any/topic").await);
    // Should NOT be able to publish (read-only)
    assert!(!auth.can_publish("reader", "any/topic").await);
}

#[tokio::test]
async fn test_registration_with_limited_role() {
    use crate::service::DefaultUserRole;

    let config = AuthConfig::default()
        .with_registration(true)
        .with_default_role(DefaultUserRole::LimitedAccess(vec![
            TopicPermission::full_access("public/#"),
            TopicPermission::read_only("announcements/*"),
        ]));
    let auth = AuthService::new(config).await;

    auth.register_user("limited", "password123").await.unwrap();

    // Should have access to public topics
    assert!(auth.can_subscribe("limited", "public/chat").await);
    assert!(auth.can_publish("limited", "public/chat").await);

    // Should have read-only access to announcements
    assert!(auth.can_subscribe("limited", "announcements/news").await);
    assert!(!auth.can_publish("limited", "announcements/news").await);

    // Should NOT have access to other topics
    assert!(!auth.can_subscribe("limited", "private/secret").await);
    assert!(!auth.can_publish("limited", "private/secret").await);
}
