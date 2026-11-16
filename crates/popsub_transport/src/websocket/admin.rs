//! The admin requests: creating, reading, changing and deleting accounts.
//!
//! Every request first checks, against the user store, that the caller is
//! an enabled admin; anyone else gets `forbidden`. Changes take effect on
//! live connections straight away: disabling or deleting an account closes
//! its connections with `CloseCode::AccountDisabled`, and a role change or
//! revoke ends the account's live subscriptions it no longer allows (each
//! such connection is told with an `unsubscribed` frame). The last enabled
//! admin can't be disabled, deleted or demoted, so the server can't be left
//! without one.

use popsub_auth::{AuthError, Role, TopicPermission, User, validate_pattern};
use popsub_protocol::{
    Access, ClientMessage, CloseCode, ErrorCode, RequestId, RoleKind, RoleSpec, ServerFrame,
    ServerMessage, TopicGrant, UserInfo,
};
use tracing::{info, warn};

use super::{Flow, Session};
use popsub_utils::error::Result;

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
            warn!(client_id = %self.client_id, caller, "admin request from a non-admin");
            self.fail(
                id,
                ErrorCode::Forbidden,
                "only admins can manage accounts",
                None,
            )?;
            return Ok(Flow::Continue);
        }
        let outcome = self.run_admin(request).await;
        match outcome {
            Ok(reply) => {
                info!(client_id = %self.client_id, caller, "admin request done");
                self.reply(id, reply)?;
            }
            Err((code, message)) => {
                warn!(client_id = %self.client_id, caller, ?code, message, "admin request refused");
                self.fail(id, code, message, None)?;
            }
        }
        Ok(Flow::Continue)
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
            info!(client_id = %client_id, username, topic, "ended a subscription the user may no longer read");
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
