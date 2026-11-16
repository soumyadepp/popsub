//! JSON frames exchanged between WebSocket clients and the server.
//!
//! Every frame is an object tagged by its `type` field. Delivered records and
//! the `subscribed`, `truncated` and `stalled` notices are written by the
//! broker, which serialises its own `popsub_broker::TopicFrame` with the
//! same tags; `ServerMessage` wraps the same structs, so clients parse every
//! server frame through this one enum.

use popsub_broker::{Delivery, Stalled, StartFrom, Subscribed, Truncated};
use serde::{Deserialize, Serialize};

/// A frame sent by a client, tagged by its `type` field.
///
/// Only `login`, `register` and `auth` are accepted before the connection has
/// authenticated. Any other frame sent first closes the connection.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Authenticates the connection with a JWT obtained from `login`.
    ///
    /// With a `session_id`, the connection holds that durable session of the
    /// user: its subscriptions and cursors are restored now and kept after
    /// the connection ends. `clean_start` discards them first. Without a
    /// `session_id` the connection is ephemeral and `clean_start` is ignored.
    #[serde(rename = "auth")]
    Auth {
        token: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        clean_start: bool,
    },
    /// Exchanges a username and password for a JWT.
    #[serde(rename = "login")]
    Login { username: String, password: String },
    /// Creates a user account, if registration is enabled on the server.
    #[serde(rename = "register")]
    Register { username: String, password: String },
    /// Starts delivery of a topic's messages to this connection, after
    /// replaying the history `from` asks for: `"resume"`, `"new"`,
    /// `{"last": N}`, `{"since_ms": T}` or `{"seq": S}`. Without `from` it is
    /// `"resume"`, which continues a durable session's stored subscription
    /// and replays nothing otherwise.
    #[serde(rename = "subscribe")]
    Subscribe {
        topic: String,
        #[serde(default)]
        from: StartFrom,
    },
    /// Stops delivery of a topic's messages to this connection.
    #[serde(rename = "unsubscribe")]
    Unsubscribe { topic: String },
    /// Publishes a payload to a topic. A missing `message_id` is replaced with
    /// a random UUID and a missing `qos` means QoS 0.
    #[serde(rename = "publish")]
    Publish {
        topic: String,
        payload: String,
        message_id: Option<String>,
        qos: Option<u8>,
    },
    /// Acknowledges a QoS 1 record delivered to this connection, so it isn't
    /// resent and the session's cursor can move past it.
    #[serde(rename = "ack")]
    Ack { topic: String, seq: u64 },
}

/// A frame sent by the server, tagged by its `type` field.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// The JWT issued after a successful `login`.
    #[serde(rename = "login_response")]
    LoginResponse { token: String },
    /// The outcome of a `register` request, with a message safe to show the user.
    #[serde(rename = "register_response")]
    RegisterResponse { success: bool, message: String },
    /// Confirms that an `auth` token was accepted. For a durable session it
    /// echoes the `session_id` and lists the topics whose subscriptions are
    /// being restored; each is followed by its own `subscribed` frame. Both
    /// fields are left out for an ephemeral connection.
    #[serde(rename = "authenticated")]
    Authenticated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed: Vec<String>,
    },
    /// Confirms that a QoS 1 publish is stored, with the id it was stored
    /// under and its `seq`. A retried publish gets the original `seq`.
    #[serde(rename = "published")]
    Published { message_id: String, seq: u64 },
    /// A failed request, rate limit or authorization denial.
    #[serde(rename = "error")]
    Error { message: String },
    /// A record from a subscribed topic, live or replayed, with its `seq`.
    #[serde(rename = "message")]
    Message(Delivery),
    /// Records the subscriber expected were trimmed before it got them.
    #[serde(rename = "truncated")]
    Truncated(Truncated),
    /// A subscription started. Records `replay_from` to `replay_to` are
    /// history; later ones are live.
    #[serde(rename = "subscribed")]
    Subscribed(Subscribed),
    /// A QoS 1 record ran out of retries and delivery on the topic is paused
    /// until the client acknowledges something or subscribes again.
    #[serde(rename = "stalled")]
    Stalled(Stalled),
}
