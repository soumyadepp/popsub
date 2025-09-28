use serde::{Deserialize, Serialize};

/// Represents messages sent **from the client to the server** in the Pub/Sub system.
///
/// This enum defines the various types of requests a client can send to the server
/// over a WebSocket connection. It is designed to be deserialized from JSON messages
/// where a `"type"` field determines the variant.
///
/// # Variants
///
/// - `Subscribe`: Instructs the server to add the client to a topic's subscriber list.
/// - `Unsubscribe`: Instructs the server to remove the client from a topic's subscriber list.
/// - `Publish`: Sends a message with a payload to a specific topic, which the server
///   will then broadcast to all active subscribers of that topic.
///
/// # JSON Format Examples
///
/// ```json
/// // Subscribe to the "chat" topic
/// { "type": "subscribe", "topic": "chat" }
///
/// // Unsubscribe from the "chat" topic
/// { "type": "unsubscribe", "topic": "chat" }
///
/// // Publish a message to the "chat" topic
/// {
///   "type": "publish",
///   "topic": "chat",
///   "payload": "Hello everyone!"
/// }
/// ```
///
/// # Usage
///
/// This enum is primarily used by the WebSocket server to parse and handle
/// incoming client requests.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Represents a client's request to authenticate.
    #[serde(rename = "auth")]
    Auth {
        /// The authentication token.
        token: String,
    },

    /// Represents a client's request to login.
    #[serde(rename = "login")]
    Login {
        /// The username.
        username: String,
        /// The password.
        password: String,
    },

    /// Represents a client's request to subscribe to a specific topic.
    #[serde(rename = "subscribe")]
    Subscribe {
        /// The name of the topic the client wishes to subscribe to.
        topic: String,
    },

    /// Represents a client's request to unsubscribe from a specific topic.
    #[serde(rename = "unsubscribe")]
    Unsubscribe {
        /// The name of the topic the client wishes to unsubscribe from.
        topic: String,
    },

    /// Represents a client's request to publish a message to a specific topic.
    #[serde(rename = "publish")]
    Publish {
        /// The name of the topic to which the message should be published.
        topic: String,
        /// The actual content of the message, typically a JSON-encoded string.
        payload: String,
        /// Optional message ID for QoS.
        message_id: Option<String>,
        /// Optional Quality of Service level.
        qos: Option<u8>,
    },

    /// Represents a client's acknowledgement of a received message.
    #[serde(rename = "ack")]
    Ack {
        /// The ID of the message being acknowledged.
        message_id: String,
    },
}

/// Represents messages sent **from the server to the client**.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Response to a successful login attempt, containing an authentication token.
    #[serde(rename = "login_response")]
    LoginResponse {
        /// The authentication token to be used in subsequent authenticated requests.
        token: String,
    },
    /// Indicates successful authentication after presenting a valid token.
    #[serde(rename = "authenticated")]
    Authenticated {},
    /// Indicates an error occurred, with a descriptive message.
    #[serde(rename = "error")]
    Error {
        /// A description of the error that occurred.
        message: String,
    },
    /// Represents a message published to a topic.
    #[serde(rename = "message")]
    Message {
        /// The topic to which the message was published.
        topic: String,
        /// The payload of the message.
        payload: String,
        /// The timestamp when the message was published.
        timestamp: i64,
        /// The ID of the message.
        message_id: String,
        /// The Quality of Service level of the message.
        qos: u8,
    },
}

/// Represents the claims of a JWT.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// The subject of the token (the user ID).
    pub sub: String,
    /// The expiration time of the token.
    pub exp: usize,
}
