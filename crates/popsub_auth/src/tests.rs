//! Unit tests that drive `AuthService` end to end: login, tokens, registration, roles and topic checks.

use crate::error::AuthError;
use crate::permission::TopicPermission;
use crate::service::{AuthConfig, AuthService, UserBuilder};
use crate::user::{Role, User};

/// Checks that the configured admin can log in, gets a token with the admin role, and can use any topic.
#[tokio::test]
async fn test_full_auth_flow() {
    let config = AuthConfig::new("test_secret").with_admin("admin", "admin123");

    let auth = AuthService::new(config).await;

    let token = auth.login("admin", "admin123").await.unwrap();
    assert!(!token.is_empty());

    let claims = auth.validate_token(&token).unwrap();
    assert_eq!(claims.sub, "admin");
    assert_eq!(claims.role, Some("admin".to_string()));

    assert!(auth.can_subscribe("admin", "any/topic").await);
    assert!(auth.can_publish("admin", "any/topic").await);
}

/// Checks that login fails for a wrong password and for an unknown user.
#[tokio::test]
async fn test_invalid_login() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let result = auth.login("admin", "wrong_password").await;
    assert!(result.is_err());

    let result = auth.login("nonexistent", "password").await;
    assert!(result.is_err());
}

/// Checks that a user role grants exactly its full-access and subscribe-only patterns.
#[tokio::test]
async fn test_user_with_limited_permissions() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let role = Role::User {
        allowed_topics: vec![
            TopicPermission::full_access("chat/#"),
            TopicPermission::read_only("sensors/+/temperature"),
        ],
    };
    auth.add_user("alice", "alice123", role).await.unwrap();

    let token = auth.login("alice", "alice123").await.unwrap();
    let claims = auth.validate_token(&token).unwrap();
    assert_eq!(claims.sub, "alice");

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
    );

    assert!(!auth.can_subscribe("alice", "other/topic").await);
    assert!(!auth.can_publish("alice", "other/topic").await);
}

/// Checks that a read-only user can subscribe to its patterns but never publish.
#[tokio::test]
async fn test_readonly_user() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let role = Role::ReadOnly {
        allowed_topics: vec!["news/#".to_string(), "updates/#".to_string()],
    };
    auth.add_user("reader", "reader123", role).await.unwrap();

    assert!(auth.can_subscribe("reader", "news/tech").await);
    assert!(auth.can_subscribe("reader", "updates/v1").await);
    assert!(!auth.can_publish("reader", "news/tech").await);
    assert!(!auth.can_publish("reader", "updates/v1").await);
}

/// Checks that `UserBuilder` grants publish and subscribe for `allow_topic` and subscribe only for `allow_subscribe`.
#[tokio::test]
async fn test_user_builder() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    UserBuilder::new("bob", "bob123")
        .allow_topic("chat/#")
        .allow_subscribe("sensors/#")
        .register(&auth)
        .await
        .unwrap();

    assert!(auth.can_subscribe("bob", "chat/room1").await);
    assert!(auth.can_publish("bob", "chat/room1").await);
    assert!(auth.can_subscribe("bob", "sensors/temp").await);
    assert!(!auth.can_publish("bob", "sensors/temp").await);
}

/// Checks that a `UserBuilder` admin can subscribe and publish on any topic.
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

/// Checks that a refreshed login token keeps the subject and the admin role claim.
#[tokio::test]
async fn test_token_refresh() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let token = auth.login("admin", "password").await.unwrap();
    let new_token = auth.refresh_token(&token).await.unwrap();

    let claims = auth.validate_token(&new_token).unwrap();
    assert_eq!(claims.sub, "admin");
    assert_eq!(claims.role.as_deref(), Some("admin"));
}

/// Checks that `authorize_subscribe` and `authorize_publish` succeed on granted topics and fail elsewhere.
#[tokio::test]
async fn test_authorization_errors() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    let role = Role::User {
        allowed_topics: vec![TopicPermission::full_access("allowed/#")],
    };
    auth.add_user("limited", "limited123", role).await.unwrap();

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

/// Checks adding, listing, changing the password and role of, and removing a user.
#[tokio::test]
async fn test_user_management() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    auth.add_user("newuser", "password123", Role::default())
        .await
        .unwrap();
    assert!(auth.user_exists("newuser").await.unwrap());

    let users = auth.list_users().await.unwrap();
    assert!(users.contains(&"newuser".to_string()));
    assert!(users.contains(&"admin".to_string()));

    auth.update_user_password("newuser", "newpassword")
        .await
        .unwrap();
    assert!(auth.login("newuser", "newpassword").await.is_ok());
    assert!(auth.login("newuser", "password123").await.is_err());

    auth.update_user_role("newuser", Role::Admin).await.unwrap();
    let user = auth.get_user("newuser").await.unwrap().unwrap();
    assert!(user.is_admin());

    auth.remove_user("newuser").await.unwrap();
    assert!(!auth.user_exists("newuser").await.unwrap());
}

/// Checks that a disabled user cannot log in, subscribe or publish, even as an admin.
#[tokio::test]
async fn test_disabled_user() {
    let config = AuthConfig::default();
    let auth = AuthService::new(config).await;

    auth.add_user("tobedisabled", "password", Role::Admin)
        .await
        .unwrap();

    let mut user = auth.get_user("tobedisabled").await.unwrap().unwrap();
    user.enabled = false;
    auth.user_store
        .update_user(user)
        .await
        .expect("update failed");

    assert!(auth.login("tobedisabled", "password").await.is_err());

    assert!(!auth.can_subscribe("tobedisabled", "any/topic").await);
    assert!(!auth.can_publish("tobedisabled", "any/topic").await);
}

/// Checks that a self-registered user can log in but gets no topics under the default role.
#[tokio::test]
async fn test_user_registration() {
    let config = AuthConfig::default().with_registration(true);
    let auth = AuthService::new(config).await;

    auth.register_user("newuser", "password123").await.unwrap();

    assert!(auth.user_exists("newuser").await.unwrap());

    let token = auth.login("newuser", "password123").await.unwrap();
    assert!(!token.is_empty());

    assert!(!auth.can_subscribe("newuser", "any/topic").await);
    assert!(!auth.can_publish("newuser", "any/topic").await);
}

/// Checks that the default config has registration off and refuses `register_user`.
#[tokio::test]
async fn test_default_config_is_closed() {
    let auth = AuthService::new(AuthConfig::default()).await;

    assert!(!auth.is_registration_enabled());
    assert!(auth.register_user("newuser", "password123").await.is_err());
    assert!(!auth.user_exists("newuser").await.unwrap());
}

/// Checks that registration rejects bad usernames and passwords under 8 characters and accepts valid ones.
#[tokio::test]
async fn test_username_and_password_rules() {
    let config = AuthConfig::default().with_registration(true);
    let auth = AuthService::new(config).await;

    for bad in ["has space", "semi;colon", "émile", "x".repeat(33).as_str()] {
        assert!(
            auth.register_user(bad, "password123").await.is_err(),
            "{bad}"
        );
    }
    assert!(
        auth.register_user("ok_user.name-1", "password123")
            .await
            .is_ok()
    );

    assert!(auth.register_user("shortpass", "seven77").await.is_err());
    assert!(auth.register_user("longpass", "eight888").await.is_ok());
}

/// Checks that `register_user` fails and creates nothing when registration is turned off.
#[tokio::test]
async fn test_registration_disabled() {
    let config = AuthConfig::default().with_registration(false);
    let auth = AuthService::new(config).await;

    let result = auth.register_user("newuser", "password123").await;
    assert!(result.is_err());
    assert!(!auth.user_exists("newuser").await.unwrap());
}

/// Checks that registration rejects a short username, a short password, and a duplicate username.
#[tokio::test]
async fn test_registration_validation() {
    let config = AuthConfig::default().with_registration(true);
    let auth = AuthService::new(config).await;

    let result = auth.register_user("ab", "password123").await;
    assert!(result.is_err());

    let result = auth.register_user("validuser", "12345").await;
    assert!(result.is_err());

    auth.register_user("uniqueuser", "password123")
        .await
        .unwrap();
    let result = auth.register_user("uniqueuser", "different").await;
    assert!(result.is_err());
}

/// Checks that the `ReadOnly` default role lets a registered user subscribe anywhere but not publish.
#[tokio::test]
async fn test_registration_with_readonly_role() {
    use crate::service::DefaultUserRole;

    let config = AuthConfig::default()
        .with_registration(true)
        .with_default_role(DefaultUserRole::ReadOnly);
    let auth = AuthService::new(config).await;

    auth.register_user("reader", "password123").await.unwrap();

    assert!(auth.can_subscribe("reader", "any/topic").await);
    assert!(!auth.can_publish("reader", "any/topic").await);
}

/// Checks that a `LimitedAccess` default role gives a registered user exactly the listed permissions.
#[tokio::test]
async fn test_registration_with_limited_role() {
    use crate::service::DefaultUserRole;

    let config = AuthConfig::default()
        .with_registration(true)
        .with_default_role(DefaultUserRole::LimitedAccess(vec![
            TopicPermission::full_access("public/#"),
            TopicPermission::read_only("announcements/#"),
        ]));
    let auth = AuthService::new(config).await;

    auth.register_user("limited", "password123").await.unwrap();

    assert!(auth.can_subscribe("limited", "public/chat").await);
    assert!(auth.can_publish("limited", "public/chat").await);

    assert!(auth.can_subscribe("limited", "announcements/news").await);
    assert!(!auth.can_publish("limited", "announcements/news").await);

    assert!(!auth.can_subscribe("limited", "private/secret").await);
    assert!(!auth.can_publish("limited", "private/secret").await);
}

/// Checks that `insecure_defaults` flags both published defaults and nothing once they are replaced.
#[test]
fn test_insecure_defaults_are_detected() {
    assert_eq!(AuthConfig::default().insecure_defaults().len(), 2);

    let custom = AuthConfig::new("a-long-random-secret").with_admin("admin", "not-the-default");
    assert!(custom.insecure_defaults().is_empty());
}

/// Checks that a disabled user can't refresh a token issued before they were disabled.
#[tokio::test]
async fn test_refresh_rejected_for_disabled_user() {
    let auth = AuthService::new(AuthConfig::default()).await;
    auth.add_user("leaver", "password123", Role::Admin)
        .await
        .unwrap();
    let token = auth.login("leaver", "password123").await.unwrap();

    let mut user = auth.get_user("leaver").await.unwrap().unwrap();
    user.enabled = false;
    auth.user_store.update_user(user).await.unwrap();

    assert!(matches!(
        auth.refresh_token(&token).await,
        Err(AuthError::InvalidToken)
    ));
}

/// Checks that an expired token is reported as `TokenExpired`, not a generic JWT error.
#[test]
fn test_expired_token_is_reported_as_expired() {
    let jwt = crate::jwt::JwtService::new("a-test-secret", 0);
    let mut claims = crate::jwt::Claims::new("someone", 0);
    claims.exp = claims.iat.saturating_sub(3600);
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        jwt.encoding_key(),
    )
    .unwrap();

    assert!(matches!(
        jwt.validate_token(&token),
        Err(AuthError::TokenExpired)
    ));
}

/// Checks that formatting the service for logs never includes the JWT secret or admin password.
#[tokio::test]
async fn test_debug_output_redacts_secrets() {
    let config =
        AuthConfig::new("super-secret-signing-key").with_admin("admin", "hunter2-password");
    let auth = AuthService::new(config).await;

    let printed = format!("{auth:?}");

    assert!(!printed.contains("super-secret-signing-key"), "{printed}");
    assert!(!printed.contains("hunter2-password"), "{printed}");
}

/// Opens a `FileUserStore` behind an `AuthService` with the default admin.
///
/// Args:
///     path: The users file.
///
/// Returns:
///     The service.
async fn file_backed_service(path: &std::path::Path) -> AuthService {
    let store = crate::FileUserStore::open(path).expect("open the users file");
    AuthService::with_store(
        AuthConfig::new("a-test-secret").with_admin("admin", "password"),
        std::sync::Arc::new(store),
    )
    .await
}

/// Checks that accounts, roles and logins survive reopening the users file,
/// and that the admin isn't added twice.
#[tokio::test]
async fn file_store_accounts_survive_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("users.json");
    let auth = file_backed_service(&path).await;
    auth.add_user(
        "alice",
        "alice-password",
        Role::read_only(vec!["news/#".into()]),
    )
    .await
    .expect("add alice");
    auth.update_user_role(
        "alice",
        Role::user(vec![TopicPermission::full_access("orders/#")]),
    )
    .await
    .expect("change alice's role");
    drop(auth);

    let auth = file_backed_service(&path).await;
    let mut names = auth.list_users().await.expect("list");
    names.sort();
    assert_eq!(names, vec!["admin".to_string(), "alice".to_string()]);
    assert!(auth.can_publish("alice", "orders/42").await);
    assert!(!auth.can_publish("alice", "news/today").await);
    assert!(auth.login("alice", "alice-password").await.is_ok());
}

/// Checks that a missing file is an empty store that is created on the
/// first change, and that removing an account persists.
#[tokio::test]
async fn file_store_starts_empty_and_persists_removals() {
    use crate::store::UserStore;
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("users.json");
    let store = crate::FileUserStore::open(&path).expect("open");
    assert_eq!(store.user_count().await.expect("count"), 0);
    assert!(!path.exists());
    assert!(!store.remove_user("nobody").await.expect("remove"));
    assert!(!path.exists());

    let user = crate::User::new("bob", "hash", Role::admin());
    store.add_user(user.clone()).await.expect("add");
    assert!(matches!(
        store.add_user(user).await,
        Err(AuthError::UserAlreadyExists(_))
    ));
    assert!(path.exists());
    assert!(store.remove_user("bob").await.expect("remove"));
    drop(store);
    let store = crate::FileUserStore::open(&path).expect("reopen");
    assert!(!store.user_exists("bob").await.expect("exists"));
}

/// Checks that a corrupt file or one from a newer format version is refused
/// rather than treated as empty.
#[test]
fn file_store_refuses_unreadable_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, b"{ not json").expect("write");
    assert!(matches!(
        crate::FileUserStore::open(&corrupt),
        Err(AuthError::Internal(_))
    ));
    let newer = dir.path().join("newer.json");
    std::fs::write(&newer, br#"{"version": 2, "users": []}"#).expect("write");
    assert!(matches!(
        crate::FileUserStore::open(&newer),
        Err(AuthError::Internal(_))
    ));
}

/// Checks that a change whose file can't be written is not applied.
#[tokio::test]
async fn file_store_changes_nothing_when_the_write_fails() {
    use crate::store::UserStore;
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("missing-dir").join("users.json");
    let store = crate::FileUserStore::open(&path).expect("open");
    let result = store
        .add_user(crate::User::new("carol", "hash", Role::admin()))
        .await;
    assert!(matches!(result, Err(AuthError::Internal(_))));
    assert!(!store.user_exists("carol").await.expect("exists"));
}

/// Checks that the users file, which holds password hashes, is readable by
/// its owner only.
#[cfg(unix)]
#[tokio::test]
async fn file_store_is_private_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("users.json");
    let _auth = file_backed_service(&path).await;
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Checks which topic patterns are accepted for a grant.
#[test]
fn patterns_are_validated() {
    use crate::permission::validate_pattern;
    for good in ["chat", "chat/#", "#", "sensors/+/temp", "+", "a.b-c_d/x"] {
        assert!(validate_pattern(good).is_ok(), "{good}");
    }
    let long = "x".repeat(crate::MAX_PATTERN_BYTES + 1);
    for bad in [
        "",
        "chat/#/more",
        "chat#",
        "a+b",
        "chat//x",
        "/chat",
        "tab\there",
        long.as_str(),
    ] {
        assert!(
            matches!(validate_pattern(bad), Err(AuthError::PermissionDenied(_))),
            "{bad}"
        );
    }
}

/// Checks that disabling an account refuses its logins and topics, and that
/// enabled admins are counted.
#[tokio::test]
async fn disabling_an_account_refuses_it() {
    let auth =
        AuthService::new(AuthConfig::new("a-test-secret").with_admin("admin", "password")).await;
    auth.add_user("erin", "erin-password", Role::user_full_access())
        .await
        .expect("add");
    auth.add_user("frank", "frank-password", Role::admin())
        .await
        .expect("add");
    assert_eq!(auth.enabled_admin_count().await.expect("count"), 2);
    auth.set_user_enabled("erin", false).await.expect("disable");
    assert!(matches!(
        auth.login("erin", "erin-password").await,
        Err(AuthError::InvalidCredentials)
    ));
    assert!(!auth.can_subscribe("erin", "anything").await);
    auth.set_user_enabled("frank", false)
        .await
        .expect("disable");
    assert_eq!(auth.enabled_admin_count().await.expect("count"), 1);
    auth.set_user_enabled("erin", true).await.expect("enable");
    assert!(auth.login("erin", "erin-password").await.is_ok());
    assert!(matches!(
        auth.set_user_enabled("nobody", true).await,
        Err(AuthError::UserNotFound(_))
    ));
}

/// Checks that a password change makes earlier tokens fail to refresh while
/// a token from a fresh login refreshes, and that old user records without a
/// token version read as version 0.
#[tokio::test]
async fn a_password_change_moves_the_token_version() {
    let service = AuthService::new(AuthConfig::new("a-secret-for-this-test")).await;
    service
        .add_user("vera", "first-password", Role::admin())
        .await
        .expect("add");
    let old = service
        .login("vera", "first-password")
        .await
        .expect("login");
    assert_eq!(service.validate_token(&old).expect("claims").ver, 0);
    service
        .update_user_password("vera", "second-password")
        .await
        .expect("change");
    assert!(matches!(
        service.refresh_token(&old).await,
        Err(AuthError::InvalidToken)
    ));
    let new = service
        .login("vera", "second-password")
        .await
        .expect("login again");
    assert_eq!(service.validate_token(&new).expect("claims").ver, 1);
    service.refresh_token(&new).await.expect("refresh");

    let stored: User = serde_json::from_str(
        r#"{"username":"old","password_hash":"x","role":{"type":"admin"},"enabled":true,"created_at":1}"#,
    )
    .expect("an old record");
    assert_eq!(stored.token_version, 0);
}

/// Checks that backing up a file store writes a private `users.json` that
/// opens with the same accounts, and that an in-memory store writes nothing.
#[tokio::test]
async fn file_store_backs_up_its_accounts() {
    use crate::store::UserStore;
    let dir = tempfile::tempdir().expect("temp dir");
    let store = crate::FileUserStore::open(dir.path().join("users.json")).expect("open");
    store
        .add_user(crate::User::new("bob", "hash", Role::admin()))
        .await
        .expect("add");
    let backup = dir.path().join("backup");
    std::fs::create_dir(&backup).expect("create");
    assert!(store.backup(&backup).await.expect("backup"));
    let copy = crate::FileUserStore::open(backup.join("users.json")).expect("open the copy");
    assert_eq!(copy.list_usernames().await.expect("list"), vec!["bob"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(backup.join("users.json"))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let memory = crate::InMemoryUserStore::new();
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).expect("create");
    assert!(!memory.backup(&empty).await.expect("backup"));
    assert!(std::fs::read_dir(&empty).expect("list").next().is_none());
}
