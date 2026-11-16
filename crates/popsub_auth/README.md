# popsub_auth

Authentication and authorization module for PopSub.

## Features

- **JWT Token Management**: HS256 token generation, validation, and refresh
- **Password Hashing**: Argon2id hashes with a random salt per password
- **Role-Based Access Control (RBAC)**: Subscribe and publish flags per topic pattern
- **Wildcard Topic Patterns**: MQTT-style wildcards (`+` and `#`) in topic grants
- **Pluggable User Storage**: A `UserStore` trait, with an in-memory store by default

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                       AuthService                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ JwtService  │  │  UserStore  │  │  PermissionChecker  │  │
│  │             │  │  (trait)    │  │                     │  │
│  │ - generate  │  │             │  │  - can_subscribe    │  │
│  │ - validate  │  │ - get_user  │  │  - can_publish      │  │
│  │ - refresh   │  │ - add_user  │  │                     │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## Usage

All `AuthService` methods that touch the user store are `async`. The samples below run inside an async function that returns `Result<(), AuthError>`.

### Basic Setup

```rust
use popsub_auth::{AuthService, AuthConfig, Role, TopicPermission};

let config = AuthConfig::new("your_jwt_secret")
    .with_expiration(24)
    .with_admin("admin", "secure_password");

let auth = AuthService::new(config).await;
```

### Adding Users

```rust
use popsub_auth::{Role, TopicPermission};

let role = Role::User {
    allowed_topics: vec![
        TopicPermission::full_access("chat/#"),
        TopicPermission::read_only("sensors/+/temp"),
    ],
};
auth.add_user("alice", "alice_password", role).await?;

use popsub_auth::service::UserBuilder;

UserBuilder::new("bob", "bob_password")
    .allow_topic("notifications/#")
    .allow_subscribe("public/#")
    .register(&auth)
    .await?;
```

### Authentication Flow

```rust
let token = auth.login("alice", "alice_password").await?;

let claims = auth.validate_token(&token)?;
println!("Authenticated user: {}", claims.sub);

let new_token = auth.refresh_token(&token).await?;
```

### Authorization Checks

```rust
assert!(auth.can_subscribe("alice", "chat/room1").await);
assert!(auth.can_publish("alice", "chat/room1").await);
assert!(auth.can_subscribe("alice", "sensors/living_room/temp").await);
assert!(!auth.can_publish("alice", "sensors/living_room/temp").await);

auth.authorize_subscribe("alice", "chat/room1").await?;
assert!(auth.authorize_publish("alice", "denied/topic").await.is_err());
```

## Roles

### Admin

Full access to all topics and operations:

```rust
let admin_role = Role::Admin;
```

### User

Topic-specific permissions with granular control:

```rust
let user_role = Role::User {
    allowed_topics: vec![
        TopicPermission::new("chat/#", true, true),
        TopicPermission::new("sensors/#", true, false),
    ],
};
```

### ReadOnly

Can only subscribe, never publish:

```rust
let readonly_role = Role::ReadOnly {
    allowed_topics: vec!["news/#".to_string(), "updates/#".to_string()],
};
```

## Topic Wildcards

| Pattern          | Matches                 | Example                             |
| ---------------- | ----------------------- | ----------------------------------- |
| `chat/room1`     | Exact match             | `chat/room1`                        |
| `chat/+`         | Single level            | `chat/room1`, `chat/room2`          |
| `chat/#`         | Multiple levels         | `chat/room1`, `chat/room1/messages` |
| `sensors/+/temp` | Single level in middle  | `sensors/living/temp`               |

`*` is not a wildcard. It only matches a level that is literally `*`.

## Custom User Store

The auth system uses a trait-based storage abstraction, allowing you to plug in any database backend.

### Using a Custom Store

```rust
use popsub_auth::store::UserStore;
use popsub_auth::service::{AuthConfig, AuthService};
use std::sync::Arc;

let my_store: Arc<dyn UserStore> = Arc::new(MyDatabaseStore::new());
let auth_service = AuthService::with_store(AuthConfig::default(), my_store).await;
```

### Implementing a Custom Store

Implement the `UserStore` trait for your database:

```rust
use async_trait::async_trait;
use popsub_auth::store::UserStore;
use popsub_auth::user::User;
use popsub_auth::error::{AuthError, Result};

pub struct PostgresUserStore {
    pool: sqlx::PgPool,
}

#[async_trait]
impl UserStore for PostgresUserStore {
    async fn get_user(&self, username: &str) -> Result<Option<User>> {
        // Query: SELECT * FROM users WHERE username = $1
        todo!()
    }

    async fn add_user(&self, user: User) -> Result<()> {
        // Query: INSERT INTO users (username, password_hash, role, ...) VALUES (...)
        todo!()
    }

    async fn update_user(&self, user: User) -> Result<()> {
        // Query: UPDATE users SET ... WHERE username = $1
        todo!()
    }

    async fn remove_user(&self, username: &str) -> Result<bool> {
        // Query: DELETE FROM users WHERE username = $1
        todo!()
    }

    async fn user_exists(&self, username: &str) -> Result<bool> {
        // Query: SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)
        todo!()
    }

    async fn list_usernames(&self) -> Result<Vec<String>> {
        // Query: SELECT username FROM users
        todo!()
    }

    async fn user_count(&self) -> Result<usize> {
        // Query: SELECT COUNT(*) FROM users
        todo!()
    }
}
```

### Available Backends

| Backend    | Status      | Notes                             |
| ---------- | ----------- | --------------------------------- |
| In-Memory  | Built in    | Default, for development and tests |
| PostgreSQL | Built in    | `PostgresUserStore`, behind the `postgres` feature |
| SQLite     | Example     | Implement with `rusqlite`         |
| Redis      | Example     | Implement with `redis-rs`         |

### PostgreSQL setup

1. Build with the feature: `cargo build -p popsub_auth --features postgres`.
2. Create the store with `PostgresUserStore::new_with_migrations(pool)`, which creates the `users` table if it is missing. If you manage schema yourself, run `migrations/001_create_users_table.sql` and use `PostgresUserStore::new(pool)` instead.
3. Pass the store to `AuthService::with_store(config, store).await`, which also creates the configured admin if it's missing.

CI compiles this store (`--all-features`) but does not run it against a database, so test it against your own Postgres before relying on it.

## Registration defaults

Registration is off unless you turn it on with `AuthConfig::with_registration(true)`. Users who register get no topics until an admin grants them some, unless you choose a role with `with_default_role`:

- `DefaultUserRole::LimitedAccess(vec![])`: no topics (the default)
- `DefaultUserRole::ReadOnly`: subscribe to every topic
- `DefaultUserRole::FullAccess`: publish and subscribe to every topic

Usernames must be 3 to 32 letters, digits, `_`, `.` or `-`. Passwords must be at least 8 characters.

## Security Considerations

- **Password Hashing**: Passwords are hashed with Argon2id, the algorithm OWASP recommends
- **JWT Secrets**: Use a long random secret. The published defaults are reported by `AuthConfig::insecure_defaults`
- **Token Expiration**: Tokens last 24 hours by default; set `with_expiration` to suit your clients. Removing or disabling a user does not revoke tokens already issued, but topic checks refuse that user straight away
- **HTTPS**: Always use TLS in production to protect tokens in transit

## License

Apache-2.0
