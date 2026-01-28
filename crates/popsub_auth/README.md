# popsub_auth

Authentication and authorization module for PopSub.

## Features

- **JWT Token Management**: Secure token generation, validation, and refresh
- **Password Hashing**: Argon2-based password hashing for secure credential storage
- **Role-Based Access Control (RBAC)**: Fine-grained permissions for topics
- **Wildcard Topic Patterns**: MQTT-style wildcards (`+`, `#`, `*`) for flexible authorization
- **Pluggable User Storage**: Trait-based storage abstraction with in-memory default

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                       AuthService                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ JwtService  │  │  UserStore  │  │  PermissionChecker  │  │
│  │             │  │  (trait)    │  │                     │  │
│  │ - generate  │  │             │  │  - can_subscribe    │  │
│  │ - validate  │  │ - get_user  │  │  - can_publish      │  │
│  │ - refresh   │  │ - add_user  │  │  - can_admin        │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## Usage

### Basic Setup

```rust
use popsub_auth::{AuthService, AuthConfig, Role, TopicPermission};

// Create auth service with configuration
let config = AuthConfig::new("your_jwt_secret")
    .with_expiration(24) // hours
    .with_admin("admin", "secure_password");

let mut auth = AuthService::new(config);
```

### Adding Users

```rust
use popsub_auth::{Role, TopicPermission};

// Add a user with specific topic permissions
let role = Role::User {
    allowed_topics: vec![
        TopicPermission::full_access("chat/#"),      // read + write
        TopicPermission::read_only("sensors/+/temp"), // read only
    ],
};
auth.add_user("alice", "alice_password", role).unwrap();

// Or use the builder pattern
use popsub_auth::service::UserBuilder;

UserBuilder::new("bob", "bob_password")
    .allow_topic("notifications/#")
    .allow_subscribe("public/*")
    .register(&mut auth)
    .unwrap();
```

### Authentication Flow

```rust
// Login and get a token
let token = auth.login("alice", "alice_password").unwrap();

// Validate the token
let claims = auth.validate_token(&token).unwrap();
println!("Authenticated user: {}", claims.sub);

// Refresh token before expiry
let new_token = auth.refresh_token(&token).unwrap();
```

### Authorization Checks

```rust
// Check permissions
assert!(auth.can_subscribe("alice", "chat/room1"));
assert!(auth.can_publish("alice", "chat/room1"));
assert!(auth.can_subscribe("alice", "sensors/living_room/temp"));
assert!(!auth.can_publish("alice", "sensors/living_room/temp")); // read-only

// Or use authorization methods that return errors
auth.authorize_subscribe("alice", "chat/room1")?;
auth.authorize_publish("alice", "denied/topic")?; // Returns Err
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
        TopicPermission::new("chat/#", true, true),   // subscribe + publish
        TopicPermission::new("sensors/*", true, false), // subscribe only
    ],
};
```

### ReadOnly

Can only subscribe, never publish:

```rust
let readonly_role = Role::ReadOnly {
    allowed_topics: vec!["news/#".to_string(), "updates/*".to_string()],
};
```

## Topic Wildcards

| Pattern          | Matches                 | Example                             |
| ---------------- | ----------------------- | ----------------------------------- |
| `chat/room1`     | Exact match             | `chat/room1`                        |
| `chat/+`         | Single level            | `chat/room1`, `chat/room2`          |
| `chat/#`         | Multiple levels         | `chat/room1`, `chat/room1/messages` |
| `chat/*`         | Multiple levels (alias) | Same as `#`                         |
| `sensors/+/temp` | Single level in middle  | `sensors/living/temp`               |

## Custom User Store

The auth system uses a trait-based storage abstraction, allowing you to plug in any database backend.

### Using a Custom Store

```rust
use popsub_auth::store::UserStore;
use popsub_auth::service::{AuthConfig, AuthService};
use std::sync::Arc;

// Create your custom store
let my_store: Arc<dyn UserStore> = Arc::new(MyDatabaseStore::new());

// Use it with AuthService
let config = AuthConfig::default();
let auth_service = AuthService::with_store(config, my_store);
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
| In-Memory  | ✅ Built-in | Default, for dev/testing          |
| PostgreSQL | 📝 Example  | Implement with `sqlx`             |
| SQLite     | 📝 Example  | Implement with `rusqlite`         |
| Redis      | 📝 Example  | Implement with `redis-rs`         |
| Sled       | 📝 Example  | Use existing `popsub_persistence` |

## Security Considerations

- **Password Hashing**: Argon2id is used for password hashing (OWASP recommended)
- **JWT Secrets**: Use strong, randomly generated secrets in production
- **Token Expiration**: Configure appropriate expiration times
- **HTTPS**: Always use TLS in production to protect tokens in transit

## License

MIT
