//! The admin requests: creating, reading, changing and deleting accounts,
//! listing, describing and purging topics, and backing the server up.
//!
//! Every request first checks, against the user store, that the caller is
//! an enabled admin; anyone else gets `forbidden`. Changes take effect on
//! live connections straight away: disabling or deleting an account closes
//! its connections with `CloseCode::AccountDisabled`, and a role change or
//! revoke ends the account's live subscriptions it no longer allows (each
//! such connection is told with an `unsubscribed` frame). A password change
//! cancels the account's tokens and closes its connections with
//! `CloseCode::TokenExpired`, so they have to log in again. The last enabled
//! admin can't be disabled, deleted or demoted, so the server can't be left
//! without one.
//!
//! A backup runs in its own task and is answered when it finishes, so the
//! admin's connection keeps being served (heartbeats included) while files
//! are copied. One backup runs at a time. The storage backend copies the
//! topic logs and sessions, the user store writes `users.json`, and
//! `BACKUP.json` is written last: a backup directory without it is incomplete.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use popsub_auth::{AuthError, AuthService, Role, TopicPermission, User, validate_pattern};
use popsub_broker::SharedBroker;
use popsub_protocol::{
    Access, BackupInfo, ClientMessage, CloseCode, ErrorCode, RequestId, RoleKind, RoleSpec,
    ServerFrame, ServerMessage, TopicGrant, UserInfo,
};
use popsub_storage::StorageError;
use popsub_utils::metrics::{attr, metrics};
use popsub_utils::pii::pii;
use tracing::{info, warn};

use super::{Flow, Session, queue_reply};
use popsub_utils::error::Result;

/// The longest backup name, in characters.
const MAX_BACKUP_NAME_CHARS: usize = 64;

/// The file written last into a finished backup.
const BACKUP_MANIFEST: &str = "BACKUP.json";

/// Checks a backup's directory name.
///
/// Args:
///     name: The name.
///
/// Returns:
///     True for 1 to `MAX_BACKUP_NAME_CHARS` ASCII letters, digits, `_`, `.`
///     or `-` that don't start with `.` or end with `.partial`, the suffix
///     a backup in progress is written under.
fn valid_backup_name(name: &str) -> bool {
    (1..=MAX_BACKUP_NAME_CHARS).contains(&name.len())
        && !name.starts_with('.')
        && !name.ends_with(".partial")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Names a backup after the current time in UTC.
///
/// Returns:
///     A name such as `20261127T101500Z`.
fn default_backup_name() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Clears the server's "a backup is running" flag when dropped, so the flag
/// is cleared even if the backup task panics.
struct BackupSlot(Arc<AtomicBool>);

impl Drop for BackupSlot {
    /// Marks the backup as finished.
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Writes a backup: the storage backend's copy, then the accounts, then
/// `BACKUP.json`. Anything written is removed if a later step fails.
///
/// Args:
///     broker: The broker, for its storage backend.
///     auth: The auth service, for its user store.
///     dest: The backup's directory. It must not exist.
///     name: The backup's name.
///
/// Returns:
///     What was written.
///
/// Raises:
///     (ErrorCode, String): `backup_unsupported` when the storage backend
///         can't be backed up while running, `storage_unavailable` when its
///         files can't be read or written, or `internal` when the accounts
///         or the manifest can't be written.
async fn write_backup(
    broker: &SharedBroker,
    auth: &AuthService,
    dest: &Path,
    name: &str,
) -> std::result::Result<BackupInfo, (ErrorCode, String)> {
    let summary = broker.backup(dest).await.map_err(|e| match e {
        StorageError::Unsupported(_) => (
            ErrorCode::BackupUnsupported,
            "this server's storage backend can't be backed up while it runs".to_string(),
        ),
        StorageError::Unavailable(message) => (
            ErrorCode::StorageUnavailable,
            format!("the backup failed: {message}"),
        ),
        other => (ErrorCode::Internal, format!("the backup failed: {other}")),
    })?;
    let undo = |message: String| async move {
        let _ = tokio::fs::remove_dir_all(dest).await;
        (ErrorCode::Internal, message)
    };
    let users = match auth.backup_users(dest).await {
        Ok(users) => users,
        Err(e) => return Err(undo(format!("writing the accounts failed: {}", e.kind())).await),
    };
    let path = tokio::fs::canonicalize(dest)
        .await
        .unwrap_or_else(|_| dest.to_path_buf());
    let info = BackupInfo {
        name: name.to_string(),
        path: path.display().to_string(),
        topics: summary.topics,
        records: summary.records,
        bytes: summary.bytes,
        users,
    };
    let manifest = serde_json::json!({
        "name": info.name,
        "created_at_ms": chrono::Utc::now().timestamp_millis(),
        "topics": info.topics,
        "records": info.records,
        "bytes": info.bytes,
        "users": info.users,
    });
    let temp = dest.join(format!("{BACKUP_MANIFEST}.tmp"));
    let written = async {
        tokio::fs::write(&temp, manifest.to_string()).await?;
        tokio::fs::rename(&temp, dest.join(BACKUP_MANIFEST)).await
    }
    .await;
    if let Err(e) = written {
        return Err(undo(format!("writing {BACKUP_MANIFEST} failed: {e}")).await);
    }
    Ok(info)
}

/// Turns an account's role into what admins see.
///
/// Args:
///     role: The stored role.
///
/// Returns:
///     The role as the protocol describes it. Grants that allow nothing are left out.
pub(crate) fn role_spec(role: &Role) -> RoleSpec {
    match role {
        Role::Admin => RoleSpec {
            kind: RoleKind::Admin,
            topics: Vec::new(),
        },
        Role::User { allowed_topics } => RoleSpec {
            kind: RoleKind::User,
            topics: allowed_topics
                .iter()
                .filter_map(|p| {
                    let access = match (p.can_subscribe, p.can_publish) {
                        (true, true) => Access::Both,
                        (true, false) => Access::Subscribe,
                        (false, true) => Access::Publish,
                        (false, false) => return None,
                    };
                    Some(TopicGrant {
                        pattern: p.pattern.clone(),
                        access,
                    })
                })
                .collect(),
        },
        Role::ReadOnly { allowed_topics } => RoleSpec {
            kind: RoleKind::Readonly,
            topics: allowed_topics
                .iter()
                .map(|pattern| TopicGrant {
                    pattern: pattern.clone(),
                    access: Access::Subscribe,
                })
                .collect(),
        },
    }
}

/// Turns what an admin asked for into a role, checking every pattern.
///
/// Args:
///     spec: The requested role.
///
/// Returns:
///     The role to store.
///
/// Raises:
///     String: When a pattern is invalid, or a `readonly` grant allows publishing.
pub(crate) fn role_from_spec(spec: &RoleSpec) -> std::result::Result<Role, String> {
    for grant in &spec.topics {
        validate_pattern(&grant.pattern).map_err(|e| e.to_string())?;
    }
    match spec.kind {
        RoleKind::Admin => Ok(Role::admin()),
        RoleKind::User => Ok(Role::user(
            spec.topics
                .iter()
                .map(|g| {
                    TopicPermission::new(
                        g.pattern.clone(),
                        matches!(g.access, Access::Subscribe | Access::Both),
                        matches!(g.access, Access::Publish | Access::Both),
                    )
                })
                .collect(),
        )),
        RoleKind::Readonly => {
            if let Some(grant) = spec.topics.iter().find(|g| g.access != Access::Subscribe) {
                return Err(format!(
                    "a readonly account can only subscribe, but '{}' asks for more",
                    grant.pattern
                ));
            }
            Ok(Role::read_only(
                spec.topics.iter().map(|g| g.pattern.clone()).collect(),
            ))
        }
    }
}

/// Describes an account for admins, without its password hash.
///
/// Args:
///     user: The stored account.
///
/// Returns:
///     The description.
pub(crate) fn user_info(user: &User) -> UserInfo {
    UserInfo {
        username: user.username.clone(),
        role: role_spec(&user.role),
        enabled: user.enabled,
        created_at_ms: user.created_at.saturating_mul(1000),
    }
}

/// Adds or replaces one pattern in a role's grants.
///
/// Args:
///     role: The current role.
///     pattern: The pattern.
///     access: What it allows.
///
/// Returns:
///     The new role.
///
/// Raises:
///     String: For an admin (which already has every topic), or publishing
///         access for a readonly account.
fn with_grant(role: &Role, pattern: &str, access: Access) -> std::result::Result<Role, String> {
    let mut spec = role_spec(role);
    if spec.kind == RoleKind::Admin {
        return Err("admins can already use every topic".to_string());
    }
    spec.topics.retain(|g| g.pattern != pattern);
    spec.topics.push(TopicGrant {
        pattern: pattern.to_string(),
        access,
    });
    role_from_spec(&spec)
}

/// Removes one pattern from a role's grants.
///
/// Args:
///     role: The current role.
///     pattern: The pattern, exactly as granted.
///
/// Returns:
///     The new role.
///
/// Raises:
///     String: For an admin, or when the role has no such grant.
fn without_grant(role: &Role, pattern: &str) -> std::result::Result<Role, String> {
    let mut spec = role_spec(role);
    if spec.kind == RoleKind::Admin {
        return Err("admins have every topic; change the role instead".to_string());
    }
    let before = spec.topics.len();
    spec.topics.retain(|g| g.pattern != pattern);
    if spec.topics.len() == before {
        return Err(format!("the account has no grant for '{pattern}'"));
    }
    role_from_spec(&spec)
}

/// Maps an auth error from an admin request to the code and text the
/// client gets.
///
/// Args:
///     error: The error.
///
/// Returns:
///     The error code and message.
fn refusal(error: &AuthError) -> (ErrorCode, String) {
    match error {
        AuthError::UserNotFound(name) => (
            ErrorCode::UserNotFound,
            format!("no account named '{name}'"),
        ),
        AuthError::UserAlreadyExists(name) => (
            ErrorCode::UserExists,
            format!("an account named '{name}' already exists"),
        ),
        AuthError::PermissionDenied(reason) => (ErrorCode::BadRequest, reason.clone()),
        _ => (ErrorCode::Internal, "the user store failed".to_string()),
    }
}

/// Maps a storage failure in a topic admin request to what the client gets.
///
/// Args:
///     error: The storage error.
///
/// Returns:
///     `storage_unavailable` with a fixed message; the detail stays in the log.
fn storage_refusal(error: popsub_storage::StorageError) -> (ErrorCode, String) {
    warn!(%error, "a topic admin request couldn't read storage");
    (
        ErrorCode::StorageUnavailable,
        "storage unavailable".to_string(),
    )
}

/// Builds the `topic_info` reply, or `topic_not_found`.
///
/// Args:
///     found: The topic, if it has a log.
///     topic: The name asked for.
///
/// Returns:
///     The reply.
///
/// Raises:
///     (ErrorCode, String): `topic_not_found` when nothing was ever published to it.
fn topic_reply(
    found: Option<popsub_protocol::TopicInfo>,
    topic: &str,
) -> std::result::Result<ServerMessage, (ErrorCode, String)> {
    found
        .map(|topic| ServerMessage::TopicInfo { topic })
        .ok_or_else(|| {
            (
                ErrorCode::TopicNotFound,
                format!("no topic named '{topic}'"),
            )
        })
}

impl Session {
    /// Handles an admin request.
    ///
    /// Args:
    ///     id: The request id.
    ///     request: The admin request; any other message is ignored.
    ///
    /// Returns:
    ///     A closing flow when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    pub(super) async fn admin(
        &self,
        id: Option<&RequestId>,
        request: ClientMessage,
    ) -> Result<Flow> {
        let caller = match self.require_user(id, "admin")? {
            Ok(caller) => caller,
            Err(flow) => return Ok(flow),
        };
        let is_admin = matches!(
            self.server.auth.get_user(&caller).await,
            Ok(Some(user)) if user.enabled && user.is_admin()
        );
        if !is_admin {
            warn!(client_id = %self.client_id, caller = %pii(&caller), "admin request from a non-admin");
            self.fail(
                id,
                ErrorCode::Forbidden,
                "only admins can manage accounts",
                None,
            )?;
            return Ok(Flow::Continue);
        }
        if let ClientMessage::Backup { name } = request {
            self.start_backup(id, &caller, name)?;
            return Ok(Flow::Continue);
        }
        let outcome = self.run_admin(request).await;
        match outcome {
            Ok(reply) => {
                info!(client_id = %self.client_id, caller = %pii(&caller), "admin request done");
                self.reply(id, reply)?;
            }
            Err((code, message)) => {
                warn!(client_id = %self.client_id, caller = %pii(&caller), code = code.as_str(), "admin request refused");
                self.fail(id, code, message, None)?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Checks a backup request and starts it in its own task, which answers
    /// with `backup_created` or an error when it finishes.
    ///
    /// Args:
    ///     id: The request id to answer with.
    ///     caller: The admin who asked, for the log.
    ///     name: The backup's name, or `None` for the current time.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a refusal can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn start_backup(
        &self,
        id: Option<&RequestId>,
        caller: &str,
        name: Option<String>,
    ) -> Result<()> {
        let name = name.unwrap_or_else(default_backup_name);
        if !valid_backup_name(&name) {
            return self.fail(
                id,
                ErrorCode::BadRequest,
                format!(
                    "a backup name is 1 to {MAX_BACKUP_NAME_CHARS} letters, digits, '_', '.' or '-', not starting with '.'"
                ),
                None,
            );
        }
        let dest = self.server.backup_dir.join(&name);
        if dest.exists() || dest.with_file_name(format!("{name}.partial")).exists() {
            return self.fail(
                id,
                ErrorCode::BackupExists,
                format!("a backup named {name} already exists"),
                None,
            );
        }
        if self.server.backup_running.swap(true, Ordering::AcqRel) {
            return self.fail(
                id,
                ErrorCode::StorageUnavailable,
                "another backup is running; try again when it finishes",
                None,
            );
        }
        let slot = BackupSlot(self.server.backup_running.clone());
        let broker = self.server.broker.clone();
        let auth = self.server.auth.clone();
        let outbox = self.outbox.clone();
        let client_id = self.client_id.clone();
        let caller = caller.to_string();
        let id = id.cloned();
        info!(client_id = %client_id, caller = %pii(&caller), backup = %name, "backup started");
        tokio::spawn(async move {
            let _slot = slot;
            let reply = match write_backup(&broker, &auth, &dest, &name).await {
                Ok(backup) => {
                    info!(client_id = %client_id, caller = %pii(&caller), backup = %backup.name, topics = backup.topics, records = backup.records, bytes = backup.bytes, "backup written");
                    ServerMessage::BackupCreated { backup }
                }
                Err((code, message)) => {
                    warn!(client_id = %client_id, caller = %pii(&caller), backup = %name, code = code.as_str(), "backup failed");
                    metrics().errors.add(1, &attr("code", code.as_str()));
                    ServerMessage::Error {
                        code,
                        message,
                        retry_after: None,
                    }
                }
            };
            let _ = queue_reply(&outbox, id, reply);
        });
        Ok(())
    }

    /// Carries out an admin request for an admin caller.
    ///
    /// Args:
    ///     request: The request.
    ///
    /// Returns:
    ///     The reply.
    ///
    /// Raises:
    ///     (ErrorCode, String): The code and message to refuse it with.
    async fn run_admin(
        &self,
        request: ClientMessage,
    ) -> std::result::Result<ServerMessage, (ErrorCode, String)> {
        let auth = &self.server.auth;
        let bad = |message: String| (ErrorCode::BadRequest, message);
        match request {
            ClientMessage::CreateUser {
                username,
                password,
                role,
            } => {
                popsub_auth::service::validate_username(&username).map_err(|e| refusal(&e))?;
                popsub_auth::service::validate_password(&password).map_err(|e| refusal(&e))?;
                let role = role_from_spec(&role).map_err(bad)?;
                auth.add_user(&username, &password, role)
                    .await
                    .map_err(|e| refusal(&e))?;
                self.user_reply(&username).await
            }
            ClientMessage::GetUser { username } => self.user_reply(&username).await,
            ClientMessage::ListUsers {} => {
                let mut names = auth.list_users().await.map_err(|e| refusal(&e))?;
                names.sort();
                let mut users = Vec::with_capacity(names.len());
                for name in names {
                    if let Some(user) = auth.get_user(&name).await.map_err(|e| refusal(&e))? {
                        users.push(user_info(&user));
                    }
                }
                Ok(ServerMessage::Users { users })
            }
            ClientMessage::SetRole { username, role } => {
                let role = role_from_spec(&role).map_err(bad)?;
                if !matches!(role, Role::Admin) {
                    self.keep_an_admin(&username).await?;
                }
                auth.update_user_role(&username, role)
                    .await
                    .map_err(|e| refusal(&e))?;
                self.end_disallowed_subscriptions(&username).await;
                self.user_reply(&username).await
            }
            ClientMessage::Grant {
                username,
                pattern,
                access,
            } => {
                let user = self.existing_user(&username).await?;
                let role = with_grant(&user.role, &pattern, access).map_err(bad)?;
                auth.update_user_role(&username, role)
                    .await
                    .map_err(|e| refusal(&e))?;
                self.end_disallowed_subscriptions(&username).await;
                self.user_reply(&username).await
            }
            ClientMessage::Revoke { username, pattern } => {
                let user = self.existing_user(&username).await?;
                let role = without_grant(&user.role, &pattern).map_err(bad)?;
                auth.update_user_role(&username, role)
                    .await
                    .map_err(|e| refusal(&e))?;
                self.end_disallowed_subscriptions(&username).await;
                self.user_reply(&username).await
            }
            ClientMessage::SetPassword { username, password } => {
                popsub_auth::service::validate_password(&password).map_err(|e| refusal(&e))?;
                auth.update_user_password(&username, &password)
                    .await
                    .map_err(|e| refusal(&e))?;
                self.server
                    .broker
                    .close_user(&username, CloseCode::TokenExpired);
                self.user_reply(&username).await
            }
            ClientMessage::SetEnabled { username, enabled } => {
                if !enabled {
                    self.keep_an_admin(&username).await?;
                }
                auth.set_user_enabled(&username, enabled)
                    .await
                    .map_err(|e| refusal(&e))?;
                if !enabled {
                    self.server
                        .broker
                        .close_user(&username, CloseCode::AccountDisabled);
                }
                self.user_reply(&username).await
            }
            ClientMessage::DeleteUser { username } => {
                self.keep_an_admin(&username).await?;
                if !auth.remove_user(&username).await.map_err(|e| refusal(&e))? {
                    return Err(refusal(&AuthError::UserNotFound(username)));
                }
                self.server
                    .broker
                    .close_user(&username, CloseCode::AccountDisabled);
                Ok(ServerMessage::UserDeleted { username })
            }
            ClientMessage::ListTopics {} => self
                .server
                .broker
                .topic_infos()
                .await
                .map(|topics| ServerMessage::Topics { topics })
                .map_err(storage_refusal),
            ClientMessage::GetTopic { topic } => {
                let found = self
                    .server
                    .broker
                    .topic_info(&topic)
                    .await
                    .map_err(storage_refusal)?;
                topic_reply(found, &topic)
            }
            ClientMessage::PurgeTopic { topic } => {
                let purged = self
                    .server
                    .broker
                    .purge_topic(&topic)
                    .await
                    .map_err(storage_refusal)?;
                topic_reply(purged, &topic)
            }
            _ => Err(bad("not an admin request".to_string())),
        }
    }

    /// Loads an account, or refuses with `user_not_found`.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Returns:
    ///     The account.
    ///
    /// Raises:
    ///     (ErrorCode, String): `user_not_found`, or `internal` for a store failure.
    async fn existing_user(
        &self,
        username: &str,
    ) -> std::result::Result<User, (ErrorCode, String)> {
        self.server
            .auth
            .get_user(username)
            .await
            .map_err(|e| refusal(&e))?
            .ok_or_else(|| refusal(&AuthError::UserNotFound(username.to_string())))
    }

    /// Builds the `user` reply for an account as it now stands.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Returns:
    ///     The reply.
    ///
    /// Raises:
    ///     (ErrorCode, String): `user_not_found`, or `internal` for a store failure.
    async fn user_reply(
        &self,
        username: &str,
    ) -> std::result::Result<ServerMessage, (ErrorCode, String)> {
        let user = self.existing_user(username).await?;
        Ok(ServerMessage::User {
            user: user_info(&user),
        })
    }

    /// Refuses a change that would leave no enabled admin: disabling,
    /// deleting or demoting the last one.
    ///
    /// Args:
    ///     username: The account the change targets.
    ///
    /// Raises:
    ///     (ErrorCode, String): `bad_request` when it is the last enabled
    ///         admin, `user_not_found`, or `internal` for a store failure.
    async fn keep_an_admin(&self, username: &str) -> std::result::Result<(), (ErrorCode, String)> {
        let user = self.existing_user(username).await?;
        if !(user.enabled && user.is_admin()) {
            return Ok(());
        }
        let admins = self
            .server
            .auth
            .enabled_admin_count()
            .await
            .map_err(|e| refusal(&e))?;
        if admins <= 1 {
            return Err((
                ErrorCode::BadRequest,
                format!("'{username}' is the last enabled admin; make another admin first"),
            ));
        }
        Ok(())
    }

    /// Ends a user's live subscriptions that its role no longer allows, and
    /// tells each affected connection with an `unsubscribed` frame. A durable
    /// session's stored subscription is forgotten too.
    ///
    /// Args:
    ///     username: The user whose role changed.
    async fn end_disallowed_subscriptions(&self, username: &str) {
        let broker = &self.server.broker;
        let live = broker.with(|b| b.subscriptions_of_user(username));
        for (client_id, topic) in live {
            if self.server.auth.can_subscribe(username, &topic).await {
                continue;
            }
            if let Err(e) = broker.unsubscribe(&topic, &client_id).await {
                warn!(client_id = %client_id, topic, %e, "forgetting a revoked subscription failed");
            }
            let notice = ServerFrame::event(ServerMessage::Unsubscribed {
                topic: topic.clone(),
            });
            broker.with(|b| b.send_frame(&client_id, &notice));
            info!(client_id = %client_id, username = %pii(username), topic, "ended a subscription the user may no longer read");
        }
    }
}

/// Unit tests for converting roles between the store and the protocol.
#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that roles survive a round trip through the protocol form.
    #[test]
    fn roles_round_trip() {
        let roles = [
            Role::admin(),
            Role::user(vec![
                TopicPermission::full_access("orders/#"),
                TopicPermission::read_only("news/+"),
                TopicPermission::write_only("events"),
            ]),
            Role::read_only(vec!["metrics/#".to_string()]),
        ];
        for role in roles {
            assert_eq!(role_from_spec(&role_spec(&role)), Ok(role.clone()));
        }
    }

    /// Checks that invalid patterns and publishing readonly grants are refused.
    #[test]
    fn invalid_role_specs_are_refused() {
        let readonly_publish = RoleSpec {
            kind: RoleKind::Readonly,
            topics: vec![TopicGrant {
                pattern: "news".to_string(),
                access: Access::Both,
            }],
        };
        assert!(role_from_spec(&readonly_publish).is_err());
        let bad_pattern = RoleSpec {
            kind: RoleKind::User,
            topics: vec![TopicGrant {
                pattern: "a/#/b".to_string(),
                access: Access::Both,
            }],
        };
        assert!(role_from_spec(&bad_pattern).is_err());
    }

    /// Checks granting and revoking patterns, including the refusals.
    #[test]
    fn grants_and_revokes_edit_the_role() {
        let role = Role::user(vec![TopicPermission::read_only("news/#")]);
        let granted = with_grant(&role, "news/#", Access::Both).expect("grant");
        assert_eq!(
            granted,
            Role::user(vec![TopicPermission::full_access("news/#")])
        );
        let revoked = without_grant(&granted, "news/#").expect("revoke");
        assert_eq!(revoked, Role::user(Vec::new()));
        assert!(without_grant(&revoked, "news/#").is_err());
        assert!(with_grant(&Role::admin(), "x", Access::Both).is_err());
        assert!(with_grant(&Role::read_only(Vec::new()), "x", Access::Publish).is_err());
    }
}
