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
    /// Answered with `subscribed`, then history, then live records.
    Subscribe {
        /// The topic.
        topic: String,
        /// Where replay starts. Omitted means `resume`.
        #[serde(default)]
        from: StartFrom,
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
