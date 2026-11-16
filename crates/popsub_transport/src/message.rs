//! JSON frames exchanged between WebSocket clients and the server.
//!
//! Every frame is an object tagged by its `type` field. Delivered records and
//! `truncated` notices are written by the broker, which serialises its own
//! `popsub_broker::TopicFrame` with the same tags; `ServerMessage` wraps the
//! same structs, so clients parse every server frame through this one enum.

use popsub_broker::{Delivery, StartFrom, Truncated};
use serde::{Deserialize, Serialize};

/// A frame sent by a client, tagged by its `type` field.
///
/// Only `login`, `register` and `auth` are accepted before the connection has
/// authenticated. Any other frame sent first closes the connection.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Authenticates the connection with a JWT obtained from `login`.
    #[serde(rename = "auth")]
    Auth { token: String },
    /// Exchanges a username and password for a JWT.
    #[serde(rename = "login")]
    Login { username: String, password: String },
    /// Creates a user account, if registration is enabled on the server.
    #[serde(rename = "register")]
    Register { username: String, password: String },
    /// Starts delivery of a topic's messages to this connection, after
    /// replaying the history `from` asks for. Without `from`, nothing is
    /// replayed: `"new"`, `{"last": N}`, `{"since_ms": T}` or `{"seq": S}`.
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
    /// Acknowledges a QoS 1 message so the broker stops retrying it.
    #[serde(rename = "ack")]
    Ack { message_id: String },
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
    /// Confirms that an `auth` token was accepted.
    #[serde(rename = "authenticated")]
    Authenticated {},
    /// A failed request, rate limit or authorization denial.
    #[serde(rename = "error")]
    Error { message: String },
    /// A record from a subscribed topic, live or replayed, with its `seq`.
    #[serde(rename = "message")]
    Message(Delivery),
    /// Replay for a topic starts later than asked because older records were
    /// trimmed. Sent before the first replayed record.
    #[serde(rename = "truncated")]
    Truncated(Truncated),
}
