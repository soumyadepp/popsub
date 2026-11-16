# popsub_auth

Users, roles, password hashing, JWTs and topic permission patterns. The transport asks this crate who a user is and whether they may subscribe or publish to a topic.

## Files

- `src/service.rs`: `AuthService` (register, login, token validation, permission checks, user management) and `AuthConfig`, including the username and password rules and the published-defaults check.
- `src/jwt.rs`: `Claims` and `JwtService` (HS256 issue, validate, refresh).
- `src/permission.rs`: `TopicPermission` and `topic_matches_pattern`, the MQTT-style matcher.
- `src/store.rs`: the `UserStore` trait, `InMemoryUserStore`, password hashing with Argon2id, and `PostgresUserStore` behind the `postgres` feature.
- `src/file_store.rs`: `FileUserStore`, accounts in one JSON file (`{"version": 1, "users": [...]}`). Every change is applied to a copy, written to a temporary file, flushed and renamed into place, and only then applied in memory. The file is mode 0600 on Unix. The binary uses it as `users.json` in the storage directory.
- `src/user.rs`: `User` and `Role` (`Admin`, `User` with topic permissions, `ReadOnly`).
- `src/error.rs`: `AuthError`.

## Invariants

- Defaults are closed: `allow_registration` is false and self-registered users get no topics (`DefaultUserRole::LimitedAccess(vec![])`). Keep it that way.
- Permissions are read from the store on every check, never from the token's `role` claim, so disabling a user or changing a role takes effect immediately.
- Only `+` (one level) and `#` (the rest) are wildcards. `*` is literal.
- Registration errors that reveal whether a username exists (`UserAlreadyExists`) must not reach clients verbatim. The transport maps them to "registration failed".
- Passwords are only ever stored as Argon2id hashes.

## Gotchas

- The in-memory store is rebuilt on every start: self-registered users vanish on restart, and the admin is recreated from config.
- `AuthService` methods take `&self` and the store locks internally, so share it as `Arc<AuthService>`; don't wrap it in another lock.
- There is one way to build it: `with_store(config, store)` (or `new(config)`, which uses an in-memory store). Both create the configured admin if it's missing.
- `AuthConfig`'s `Debug` redacts the secret and admin password. Keep it that way if fields are added.
- `refresh_token` checks the user still exists and is enabled; `validate_token` doesn't, because permission checks look the user up anyway.
- The Postgres store compiles in CI (`--all-features`) but is never run against a database.
- There is no protocol message to change roles; use `update_user_role` in code.

## Tests

- `make test-crate CRATE=popsub_auth`. Argon2 hashing makes these slower than the other crates.
