//! Frames a client sends.

use serde::{Deserialize, Serialize};

use crate::RequestId;

/// A client frame: an optional request `id` and the request itself.
///
/// On the wire the `id` sits beside `type` in the same object, for example
/// `{"type":"subscribe","id":7,"topic":"chat"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClientFrame {
    /// Echoed in the reply, if given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// The request.
    #[serde(flatten)]
    pub message: ClientMessage,
}

impl ClientFrame {
    /// Wraps a request with no `id`.
    ///
    /// Args:
    ///     message: The request.
    ///
    /// Returns:
    ///     The frame.
    pub fn new(message: ClientMessage) -> Self {
        Self { id: None, message }
    }

    /// Wraps a request with an `id`.
    ///
    /// Args:
    ///     id: The request id.
    ///     message: The request.
    ///
    /// Returns:
    ///     The frame.
    pub fn with_id(id: RequestId, message: ClientMessage) -> Self {
        Self {
            id: Some(id),
            message,
        }
    }
}

/// A request, tagged by its `type` field.
///
/// Only `login`, `register` and `auth` are accepted before the connection
/// has authenticated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Authenticates the connection with a JWT from `login`. Answered with
    /// `authenticated`.
    ///
    /// `protocol` is the version the client speaks; omitted means 1. With a
    /// `session_id`, the connection holds that durable session of the user:
    /// its subscriptions and cursors are restored and kept after the
    /// connection ends. `clean_start` discards them first.
    Auth {
        /// The JWT.
        token: String,
        /// The protocol version the client speaks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<u32>,
        /// The durable session to hold, 1 to 128 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Whether to discard the session's stored subscriptions.
        #[serde(default)]
        clean_start: bool,
    },
    /// Exchanges a username and password for a JWT. Answered with `logged_in`.
    Login {
        /// The username.
        username: String,
        /// The password.
        password: String,
    },
    /// Creates an account, if the server allows it. Answered with `registered`.
    Register {
        /// The requested username.
        username: String,
        /// The requested password.
        password: String,
    },
    /// Subscribes to a topic, replaying the history `from` asks for first.
    /// Answered with `subscribed`, then history, then live records. The
    /// topic may be a wildcard pattern, or the request may name a `group`
    /// to share the subscription with other connections, but not both.
    Subscribe {
        /// The topic.
        topic: String,
        /// Where replay starts. Omitted means `resume`.
        #[serde(default)]
        from: StartFrom,
        /// A shared subscription to join. Each message on the topic goes to
        /// one member of the group, and the group keeps its own position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
    },
    /// Stops delivery of a topic. Answered with `unsubscribed`.
    Unsubscribe {
        /// The topic.
        topic: String,
    },
    /// Publishes a payload. A QoS 1 publish is answered with `published`
    /// once stored; QoS 0 gets no reply unless it fails.
    Publish {
        /// The topic.
        topic: String,
        /// The message body.
        payload: String,
        /// The publisher's idempotency key; generated when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// 0 (the default) or 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        qos: Option<u8>,
    },
    /// Acknowledges one QoS 1 record delivered to this connection. No reply.
    Ack {
        /// The topic it was delivered on.
        topic: String,
        /// Its `seq`.
        seq: u64,
    },
    /// Admin only. Lists every topic that has a log. Answered with `topics`.
    ListTopics {},
    /// Admin only. Describes one topic. Answered with `topic_info`.
    GetTopic {
        /// The topic.
        topic: String,
    },
    /// Admin only. Removes every record a topic holds. Its numbering
    /// carries on, so the next publish gets the next `seq`; subscribers
    /// that were behind are told with `truncated`. Answered with `topic_info`.
    PurgeTopic {
        /// The topic.
        topic: String,
    },
    /// Asks the server to answer with `pong`, so a client that can't see
    /// WebSocket pings (a browser) can tell the connection is still alive.
    /// Accepted before `auth` too.
    Ping {},
    /// Admin only. Creates an account. Answered with `user`.
    CreateUser {
        /// The new username: 3 to 32 letters, digits, `_`, `.` or `-`.
        username: String,
        /// The new password, at least 8 characters.
        password: String,
        /// What the account may do.
        role: RoleSpec,
    },
    /// Admin only. Reads one account. Answered with `user`.
    GetUser {
        /// The account.
        username: String,
    },
    /// Admin only. Lists every account. Answered with `users`.
    ListUsers {},
    /// Admin only. Replaces an account's role. Answered with `user`. Live
    /// subscriptions the new role no longer allows are ended.
    SetRole {
        /// The account.
        username: String,
        /// Its new role.
        role: RoleSpec,
    },
    /// Admin only. Adds (or replaces) one topic pattern in a `user` or
    /// `readonly` account's role. Answered with `user`.
    Grant {
        /// The account.
        username: String,
        /// The topic pattern, with `+` for one level and a trailing `#` for the rest.
        pattern: String,
        /// What the pattern allows. A `readonly` account can only be granted `subscribe`.
        access: Access,
    },
    /// Admin only. Removes one topic pattern from an account's role. Answered
    /// with `user`. Live subscriptions it no longer allows are ended.
    Revoke {
        /// The account.
        username: String,
        /// The pattern, exactly as granted.
        pattern: String,
    },
    /// Admin only. Sets an account's password. Answered with `user`. Tokens
    /// issued before it are cancelled and the account's connections closed.
    SetPassword {
        /// The account.
        username: String,
        /// The new password, at least 8 characters.
        password: String,
    },
    /// Admin only. Enables or disables an account. Answered with `user`.
    /// Disabling closes its connections with `CloseCode::AccountDisabled`.
    SetEnabled {
        /// The account.
        username: String,
        /// Whether it may log in and use topics.
        enabled: bool,
    },
    /// Admin only. Deletes an account. Answered with `user_deleted`. Its
    /// connections are closed with `CloseCode::AccountDisabled`.
    DeleteUser {
        /// The account.
        username: String,
    },
}

impl ClientMessage {
    /// The frame's `type` on the wire, for logs, metrics and trace names.
    ///
    /// Returns:
    ///     The type, such as `publish` or `create_user`.
    pub fn kind(&self) -> &'static str {
        match self {
            ClientMessage::Auth { .. } => "auth",
            ClientMessage::Login { .. } => "login",
            ClientMessage::Register { .. } => "register",
            ClientMessage::Subscribe { .. } => "subscribe",
            ClientMessage::Unsubscribe { .. } => "unsubscribe",
            ClientMessage::Publish { .. } => "publish",
            ClientMessage::Ack { .. } => "ack",
            ClientMessage::Ping { .. } => "ping",
            ClientMessage::ListTopics { .. } => "list_topics",
            ClientMessage::GetTopic { .. } => "get_topic",
            ClientMessage::PurgeTopic { .. } => "purge_topic",
            ClientMessage::CreateUser { .. } => "create_user",
            ClientMessage::GetUser { .. } => "get_user",
            ClientMessage::ListUsers { .. } => "list_users",
            ClientMessage::SetRole { .. } => "set_role",
            ClientMessage::Grant { .. } => "grant",
            ClientMessage::Revoke { .. } => "revoke",
            ClientMessage::SetPassword { .. } => "set_password",
            ClientMessage::SetEnabled { .. } => "set_enabled",
            ClientMessage::DeleteUser { .. } => "delete_user",
        }
    }
}

/// What an account may do, as admins see and set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RoleSpec {
    /// The kind of account.
    pub kind: RoleKind,
    /// The topic patterns it may use. Ignored for `admin`, which may use
    /// every topic; every grant of a `readonly` account is `subscribe`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<TopicGrant>,
}

impl RoleSpec {
    /// An admin, which may use every topic and the admin requests.
    ///
    /// Returns:
    ///     The role.
    pub fn admin() -> Self {
        Self {
            kind: RoleKind::Admin,
            topics: Vec::new(),
        }
    }

    /// A user that may use the topics its grants allow.
    ///
    /// Args:
    ///     topics: The grants.
    ///
    /// Returns:
    ///     The role.
    pub fn user(topics: Vec<TopicGrant>) -> Self {
        Self {
            kind: RoleKind::User,
            topics,
        }
    }

    /// An account that may only subscribe, to the given patterns.
    ///
    /// Args:
    ///     patterns: The patterns it may subscribe to.
    ///
    /// Returns:
    ///     The role, with a `subscribe` grant per pattern.
    pub fn readonly<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            kind: RoleKind::Readonly,
            topics: patterns
                .into_iter()
                .map(|pattern| TopicGrant::new(pattern, Access::Subscribe))
                .collect(),
        }
    }
}

/// The kinds of account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoleKind {
    /// May use every topic and the admin requests.
    Admin,
    /// May use the topics its grants allow.
    User,
    /// May subscribe to the topics its grants allow, and never publish.
    Readonly,
}

/// One topic pattern and what it allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TopicGrant {
    /// The pattern, with `+` for one level and a trailing `#` for the rest.
    pub pattern: String,
    /// What it allows.
    pub access: Access,
}

impl TopicGrant {
    /// Builds a grant.
    ///
    /// Args:
    ///     pattern: The topic pattern.
    ///     access: What it allows.
    ///
    /// Returns:
    ///     The grant.
    pub fn new(pattern: impl Into<String>, access: Access) -> Self {
        Self {
            pattern: pattern.into(),
            access,
        }
    }
}

/// What a topic grant allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Subscribing only.
    Subscribe,
    /// Publishing only.
    Publish,
    /// Both.
    Both,
}

/// Where replay starts when a client subscribes.
///
/// On the wire this is `"resume"`, `"new"`, `{"last": 50}`,
/// `{"since_ms": 1794800000000}` or `{"seq": 1200}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StartFrom {
    /// One past the stored cursor of this session's subscription to the
    /// topic. For an ephemeral connection, or a session with no stored
    /// subscription to the topic, the same as `New`.
    #[default]
    Resume,
    /// No replay. Only records published after subscribing.
    New,
    /// The last N retained records.
    Last(u64),
    /// The first record published at or after this time, in milliseconds
    /// since the Unix epoch.
    SinceMs(i64),
    /// This `seq`, or the oldest retained one if it was trimmed.
    Seq(u64),
}
