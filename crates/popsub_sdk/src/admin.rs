//! `Admin`, the account management requests, for clients logged in as an admin.
//!
//! Each call is one request and its reply. None of them is queued while the
//! client is disconnected or resent after a reconnect, because repeating a
//! change such as `create_user` isn't safe: a call made while disconnected
//! fails with `Error::Connect`, and one whose connection drops before the
//! reply fails the same way, whether or not the server carried it out.

use popsub_protocol::{Access, ClientMessage, RoleSpec, ServerMessage, TopicInfo, UserInfo};

use crate::client::Client;
use crate::error::Error;

/// The admin requests, borrowed from a `Client` with `Client::admin`.
///
/// Every call fails with `Error::Server` and `ErrorCode::Forbidden` unless
/// the client is logged in as an enabled admin.
#[derive(Debug, Clone, Copy)]
pub struct Admin<'a> {
    /// The client the requests go through.
    client: &'a Client,
}

impl<'a> Admin<'a> {
    /// Wraps a client.
    ///
    /// Args:
    ///     client: The client.
    ///
    /// Returns:
    ///     The handle.
    pub(crate) fn new(client: &'a Client) -> Self {
        Self { client }
    }

    /// Creates an account.
    ///
    /// Args:
    ///     username: Its name.
    ///     password: Its password.
    ///     role: What it may do.
    ///
    /// Returns:
    ///     The new account.
    ///
    /// Raises:
    ///     Error::Server: `user_exists`, `bad_request` for an invalid name,
    ///         password or pattern, or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn create_user(
        &self,
        username: impl Into<String>,
        password: impl Into<String>,
        role: RoleSpec,
    ) -> Result<UserInfo, Error> {
        self.user(ClientMessage::CreateUser {
            username: username.into(),
            password: password.into(),
            role,
        })
        .await
    }

    /// Reads an account.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Returns:
    ///     The account.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found` or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn get_user(&self, username: impl Into<String>) -> Result<UserInfo, Error> {
        self.user(ClientMessage::GetUser {
            username: username.into(),
        })
        .await
    }

    /// Lists every account, sorted by name.
    ///
    /// Returns:
    ///     The accounts.
    ///
    /// Raises:
    ///     Error::Server: `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    pub async fn list_users(&self) -> Result<Vec<UserInfo>, Error> {
        match self.client.request(ClientMessage::ListUsers {}).await? {
            ServerMessage::Users { users } => Ok(users),
            other => Err(Error::Protocol(format!("{other:?}"))),
        }
    }

    /// Replaces an account's role. Live subscriptions the new role doesn't
    /// allow are ended.
    ///
    /// Args:
    ///     username: The account.
    ///     role: Its new role.
    ///
    /// Returns:
    ///     The account as it now stands.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found`, `bad_request` for an invalid
    ///         pattern or for demoting the last enabled admin, or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn set_role(
        &self,
        username: impl Into<String>,
        role: RoleSpec,
    ) -> Result<UserInfo, Error> {
        self.user(ClientMessage::SetRole {
            username: username.into(),
            role,
        })
        .await
    }

    /// Grants an account a topic pattern, replacing any grant it already
    /// has for exactly that pattern.
    ///
    /// Args:
    ///     username: The account.
    ///     pattern: The topic pattern.
    ///     access: What it allows.
    ///
    /// Returns:
    ///     The account as it now stands.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found`, `bad_request` (an invalid
    ///         pattern, an admin account, or publishing for a readonly one),
    ///         or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn grant(
        &self,
        username: impl Into<String>,
        pattern: impl Into<String>,
        access: Access,
    ) -> Result<UserInfo, Error> {
        self.user(ClientMessage::Grant {
            username: username.into(),
            pattern: pattern.into(),
            access,
        })
        .await
    }

    /// Removes one of an account's grants. Live subscriptions that are no
    /// longer allowed are ended.
    ///
    /// Args:
    ///     username: The account.
    ///     pattern: The pattern, exactly as granted.
    ///
    /// Returns:
    ///     The account as it now stands.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found`, `bad_request` when the account
    ///         has no such grant, or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn revoke(
        &self,
        username: impl Into<String>,
        pattern: impl Into<String>,
    ) -> Result<UserInfo, Error> {
        self.user(ClientMessage::Revoke {
            username: username.into(),
            pattern: pattern.into(),
        })
        .await
    }

    /// Sets an account's password. Tokens issued before it are cancelled,
    /// and the account's connections are closed so they log in again.
    ///
    /// Args:
    ///     username: The account.
    ///     password: The new password.
    ///
    /// Returns:
    ///     The account.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found`, `bad_request` for a password
    ///         that is too short or too long, or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn set_password(
        &self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<UserInfo, Error> {
        self.user(ClientMessage::SetPassword {
            username: username.into(),
            password: password.into(),
        })
        .await
    }

    /// Enables or disables an account. Disabling closes its connections
    /// with `CloseCode::AccountDisabled` and refuses its logins and tokens.
    ///
    /// Args:
    ///     username: The account.
    ///     enabled: Whether it may log in.
    ///
    /// Returns:
    ///     The account as it now stands.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found`, `bad_request` for disabling the
    ///         last enabled admin, or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn set_enabled(
        &self,
        username: impl Into<String>,
        enabled: bool,
    ) -> Result<UserInfo, Error> {
        self.user(ClientMessage::SetEnabled {
            username: username.into(),
            enabled,
        })
        .await
    }

    /// Deletes an account and closes its connections with
    /// `CloseCode::AccountDisabled`.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Raises:
    ///     Error::Server: `user_not_found`, `bad_request` for the last
    ///         enabled admin, or `forbidden`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    pub async fn delete_user(&self, username: impl Into<String>) -> Result<(), Error> {
        let username = username.into();
        match self
            .client
            .request(ClientMessage::DeleteUser { username })
            .await?
        {
            ServerMessage::UserDeleted { .. } => Ok(()),
            other => Err(Error::Protocol(format!("{other:?}"))),
        }
    }

    /// Lists every topic that has a log, sorted by name.
    ///
    /// Returns:
    ///     The topics, with their bounds, record counts and live subscribers.
    ///
    /// Raises:
    ///     Error::Server: `forbidden`, or `storage_unavailable`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    pub async fn list_topics(&self) -> Result<Vec<TopicInfo>, Error> {
        match self.client.request(ClientMessage::ListTopics {}).await? {
            ServerMessage::Topics { topics } => Ok(topics),
            other => Err(Error::Protocol(format!("{other:?}"))),
        }
    }

    /// Describes one topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     Its bounds, record count and live subscribers.
    ///
    /// Raises:
    ///     Error::Server: `topic_not_found`, `forbidden`, or `storage_unavailable`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    pub async fn get_topic(&self, topic: impl Into<String>) -> Result<TopicInfo, Error> {
        self.topic(ClientMessage::GetTopic {
            topic: topic.into(),
        })
        .await
    }

    /// Removes every record a topic holds. Its numbering carries on, and
    /// subscribers that hadn't caught up are told their history was truncated.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The topic afterwards.
    ///
    /// Raises:
    ///     Error::Server: `topic_not_found`, `forbidden`, or `storage_unavailable`.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    pub async fn purge_topic(&self, topic: impl Into<String>) -> Result<TopicInfo, Error> {
        self.topic(ClientMessage::PurgeTopic {
            topic: topic.into(),
        })
        .await
    }

    /// Sends a request answered by a `topic_info` frame.
    ///
    /// Args:
    ///     message: The request.
    ///
    /// Returns:
    ///     The topic in the reply.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    async fn topic(&self, message: ClientMessage) -> Result<TopicInfo, Error> {
        match self.client.request(message).await? {
            ServerMessage::TopicInfo { topic } => Ok(topic),
            other => Err(Error::Protocol(format!("{other:?}"))),
        }
    }

    /// Sends a request answered by a `user` frame.
    ///
    /// Args:
    ///     message: The request.
    ///
    /// Returns:
    ///     The account in the reply.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    ///     Error::Protocol: When the server answers with something else.
    async fn user(&self, message: ClientMessage) -> Result<UserInfo, Error> {
        match self.client.request(message).await? {
            ServerMessage::User { user } => Ok(user),
            other => Err(Error::Protocol(format!("{other:?}"))),
        }
    }
}
