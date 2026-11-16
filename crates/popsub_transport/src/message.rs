//! JSON frames exchanged between WebSocket clients and the server.
//!
//! Every frame is an object tagged by its `type` field. Delivered topic messages
//! are the exception: the broker serialises its own `Message` directly, without a
//! `type` field, so `ServerMessage::Message` is defined but not sent today.

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
    /// Starts delivery of a topic's messages to this connection.
    #[serde(rename = "subscribe")]
    Subscribe { topic: String },
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
    /// A topic message in tagged form. Defined but not sent: deliveries use the
    /// broker's untagged `Message` instead.
    #[serde(rename = "message")]
    Message {
        topic: String,
        payload: String,
        timestamp: i64,
        message_id: String,
        qos: u8,
    },
}
