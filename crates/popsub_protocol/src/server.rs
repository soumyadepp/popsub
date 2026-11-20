//! Frames the server sends.

use serde::{Deserialize, Serialize};

use crate::RequestId;
use crate::client::{Encoding, RoleSpec};

/// A server frame: the `id` of the request it answers, if any, and the frame.
///
/// Replies carry the request's `id` when the request had one. Frames the
/// server sends on its own (`message`, `truncated`, `stalled`, and the
/// `subscribed` frames for subscriptions a session restores) have none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ServerFrame {
    /// The `id` of the request this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// The frame.
    #[serde(flatten)]
    pub message: ServerMessage,
}

impl ServerFrame {
    /// Builds a frame the server sends on its own, with no `id`.
    ///
    /// Args:
    ///     message: The frame.
    ///
    /// Returns:
    ///     The frame.
    pub fn event(message: ServerMessage) -> Self {
        Self { id: None, message }
    }

    /// Builds a reply to a request.
    ///
    /// Args:
    ///     id: The request's `id`, if it had one.
    ///     message: The reply.
    ///
    /// Returns:
    ///     The frame.
    pub fn reply(id: Option<RequestId>, message: ServerMessage) -> Self {
        Self { id, message }
    }
}

/// A server frame, tagged by its `type` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The JWT issued for a `login`.
    LoggedIn {
        /// The token to send in `auth`.
        token: String,
    },
    /// The account from a `register` was created.
    Registered {},
    /// The answer to `ping`.
    Pong {},
    /// An `auth` was accepted. On an already authenticated connection,
    /// `auth` with a new token for the same user only refreshes the token,
    /// and the reply's `resumed` is empty.
    Authenticated {
        /// The protocol version the connection uses from now on.
        protocol: u32,
        /// The durable session held, echoed from `auth`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Topics the session has stored subscriptions for. Each is being
        /// restored and is followed by its own `subscribed` frame, unless
        /// `held` is set.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed: Vec<String>,
        /// Whether the `auth` asked for `hold_resumed` and the server
        /// honoured it: the topics in `resumed` are stored but not attached,
        /// and nothing is sent on one until the client subscribes to it.
        #[serde(default, skip_serializing_if = "is_false")]
        held: bool,
        /// Limits the client should respect.
        limits: Limits,
    },
    /// A subscription started. Records `replay_from` to `replay_to` are
    /// history; later ones are live.
    Subscribed(Subscribed),
    /// The connection no longer receives the topic. Sent in reply to
    /// `unsubscribe`, and also without an `id` when an admin's change to the
    /// account ended a subscription it no longer allows.
    Unsubscribed {
        /// The topic.
        topic: String,
    },
    /// A QoS 1 publish is stored. A retried publish gets the original `seq`.
    Published {
        /// The id it was stored under, generated if the publish had none.
        message_id: String,
        /// Its position in the topic log.
        seq: u64,
    },
    /// A record from a subscribed topic, live or replayed.
    Message(Delivery),
    /// Records the subscriber expected were trimmed before it got them.
    Truncated(Truncated),
    /// The connection's token expires soon. Send `auth` with a new token
    /// before `expires_at_ms`, or the connection is closed with
    /// `CloseCode::TokenExpired`.
    AuthExpiring {
        /// When the token expires, in milliseconds since the Unix epoch.
        expires_at_ms: i64,
    },
    /// A QoS 1 record ran out of retries; delivery on the topic is paused
    /// until the client acknowledges something or subscribes again.
    Stalled(Stalled),
    /// An account, as it stands after an admin request.
    User {
        /// The account.
        user: UserInfo,
    },
    /// Every account, sorted by username.
    Users {
        /// The accounts.
        users: Vec<UserInfo>,
    },
    /// An account was deleted.
    UserDeleted {
        /// The account.
        username: String,
    },
    /// One topic, in answer to `get_topic` and `purge_topic`.
    TopicInfo {
        /// The topic.
        topic: TopicInfo,
    },
    /// Every topic, sorted by name, in answer to `list_topics`.
    Topics {
        /// The topics.
        topics: Vec<TopicInfo>,
    },
    /// A request failed.
    Error {
        /// What went wrong, for programs.
        code: ErrorCode,
        /// What went wrong, for people.
        message: String,
        /// Seconds to wait before trying again, for rate limits and lockouts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
    },
}

/// Why a request failed. New codes may be added within a protocol version;
/// clients should treat an unknown code like `internal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The frame isn't valid JSON, has an unknown `type`, or misses a field.
    BadRequest,
    /// The request needs `auth` first. The connection is closed.
    Unauthenticated,
    /// The token was invalid or expired. The connection is closed.
    AuthFailed,
    /// The username or password was wrong.
    InvalidCredentials,
    /// Too many failed logins; wait `retry_after` seconds.
    LockedOut,
    /// Too many requests; wait `retry_after` seconds.
    RateLimited,
    /// The user may not use this topic.
    NotAuthorized,
    /// The account couldn't be created; `message` says why when it is safe to.
    RegistrationFailed,
    /// The `session_id` is empty, too long or has control characters.
    InvalidSessionId,
    /// The session store couldn't be reached. The connection is closed.
    SessionUnavailable,
    /// The server doesn't speak the requested protocol version. The
    /// connection is closed.
    UnsupportedProtocol,
    /// The message store couldn't be reached; retry later.
    StorageUnavailable,
    /// The payload is longer than `Limits::max_payload_bytes`.
    PayloadTooLarge,
    /// The topic name is empty, longer than `Limits::max_topic_bytes`, or
    /// has control characters or the wildcard characters `+` and `#`.
    InvalidTopic,
    /// Anything else went wrong on the server.
    Internal,
    /// The request needs an admin account.
    Forbidden,
    /// No account has that username.
    UserNotFound,
    /// An account with that username already exists.
    UserExists,
    /// The topic has no log: nothing was ever published to it.
    TopicNotFound,
}

impl ErrorCode {
    /// The code as it appears on the wire.
    ///
    /// Returns:
    ///     The snake_case code, such as `rate_limited`.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Unauthenticated => "unauthenticated",
            ErrorCode::AuthFailed => "auth_failed",
            ErrorCode::InvalidCredentials => "invalid_credentials",
            ErrorCode::LockedOut => "locked_out",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::NotAuthorized => "not_authorized",
            ErrorCode::RegistrationFailed => "registration_failed",
            ErrorCode::InvalidSessionId => "invalid_session_id",
            ErrorCode::SessionUnavailable => "session_unavailable",
            ErrorCode::UnsupportedProtocol => "unsupported_protocol",
            ErrorCode::StorageUnavailable => "storage_unavailable",
            ErrorCode::PayloadTooLarge => "payload_too_large",
            ErrorCode::InvalidTopic => "invalid_topic",
            ErrorCode::Internal => "internal",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::UserNotFound => "user_not_found",
            ErrorCode::UserExists => "user_exists",
            ErrorCode::TopicNotFound => "topic_not_found",
        }
    }
}

/// An account, as admins see it. Never includes the password or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UserInfo {
    /// The username.
    pub username: String,
    /// What it may do.
    pub role: RoleSpec,
    /// Whether it may log in and use topics.
    pub enabled: bool,
    /// When it was created, in milliseconds since the Unix epoch.
    pub created_at_ms: i64,
}

/// A topic, as admins see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TopicInfo {
    /// The topic's name.
    pub name: String,
    /// The oldest `seq` still kept, or `last_seq + 1` when nothing is kept.
    pub first_seq: u64,
    /// The newest `seq` ever published.
    pub last_seq: u64,
    /// How many records are kept now.
    pub messages: u64,
    /// How many live subscriptions it has.
    pub subscribers: u64,
}

/// Limits the server advertises in `authenticated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Limits {
    /// The most QoS 1 records a subscription may have unacknowledged.
    pub max_inflight: u64,
    /// How long a durable session is kept after its last connection, in seconds.
    pub session_expiry_secs: u64,
    /// How often the server sends a WebSocket ping, in seconds.
    pub heartbeat_secs: u64,
    /// How long the server waits for anything from the client, a pong
    /// included, before closing with `CloseCode::IdleTimeout`, in seconds.
    pub idle_timeout_secs: u64,
    /// The longest payload a publish may carry, in bytes.
    pub max_payload_bytes: u64,
    /// The longest topic name, in bytes.
    pub max_topic_bytes: u64,
}

/// A record delivered to a subscriber, live or replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Delivery {
    /// The topic the record was published to.
    pub topic: String,
    /// The record's position in the topic log. Consumers deduplicate by
    /// `(topic, seq)`.
    pub seq: u64,
    /// The publisher's id for the message.
    pub message_id: String,
    /// The message body.
    pub payload: String,
    /// How `payload` encodes the message's bytes, as the publisher said.
    /// Absent means the payload is the text itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<Encoding>,
    /// The delivery level the publisher asked for: 0 or 1.
    pub qos: u8,
    /// When the message was published, in milliseconds since the Unix epoch.
    pub published_at_ms: i64,
}

/// Tells a subscriber that records it expected were trimmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Truncated {
    /// The topic.
    pub topic: String,
    /// The oldest `seq` still available. Delivery continues from here.
    pub first_available: u64,
}

/// Confirms a subscription and says which records are history.
///
/// The range is empty when `replay_from` is greater than `replay_to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Subscribed {
    /// The topic.
    pub topic: String,
    /// The first `seq` that will be sent.
    pub replay_from: u64,
    /// The newest `seq` in the log when the subscription started.
    pub replay_to: u64,
    /// The shared subscription that was joined, when the request named one.
    /// `replay_from` is then the group's position, not this member's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Tells a subscriber that a QoS 1 record ran out of retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Stalled {
    /// The topic.
    pub topic: String,
    /// The record that was never acknowledged.
    pub seq: u64,
}

/// Tells serde to leave a `false` flag out of a frame.
///
/// Args:
///     flag: The flag.
///
/// Returns:
///     True when the flag is false.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(flag: &bool) -> bool {
    !*flag
}
