//! Frames the server sends.

use serde::{Deserialize, Serialize};

use crate::RequestId;

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
    /// An `auth` was accepted. On an already authenticated connection,
    /// `auth` with a new token for the same user only refreshes the token,
    /// and the reply's `resumed` is empty.
    Authenticated {
        /// The protocol version the connection uses from now on.
        protocol: u32,
        /// The durable session held, echoed from `auth`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Topics whose stored subscriptions are being restored. Each is
        /// followed by its own `subscribed` frame.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed: Vec<String>,
        /// Limits the client should respect.
        limits: Limits,
    },
    /// A subscription started. Records `replay_from` to `replay_to` are
    /// history; later ones are live.
    Subscribed(Subscribed),
    /// The connection no longer receives the topic.
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
