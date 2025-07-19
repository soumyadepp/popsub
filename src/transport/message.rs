use serde::Deserialize;

/// Represents messages sent **from the client to the server** in the Pub/Sub system.
///
/// This enum is deserialized based on the `"type"` field in incoming JSON.
/// Each variant corresponds to a supported client action.
///
/// # Variants
///
/// - `Subscribe` - Subscribe to a given topic.
/// - `Unsubscribe` - Unsubscribe from a given topic.
/// - `Publish` - Publish a message to a topic (with a payload and timestamp).
///
/// # JSON Format
///
/// ```json
/// { "type": "subscribe", "topic": "updates" }
/// { "type": "unsubscribe", "topic": "updates" }
/// { "type": "publish", "topic": "updates", "payload": "data", "timestamp": 1725000000 }
/// ```
///
/// # Usage
///
/// Used to deserialize WebSocket messages sent by clients.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Subscribe to a topic.
    #[serde(rename = "subscribe")]
    Subscribe {
        /// The topic name to subscribe to.
        topic: String,
    },

    /// Unsubscribe from a topic.
    #[serde(rename = "unsubscribe")]
    Unsubscribe {
        /// The topic name to unsubscribe from.
        topic: String,
    },

    /// Publish a message to a topic.
    #[serde(rename = "publish")]
    Publish {
        /// The target topic.
        topic: String,
        /// The actual message payload (usually JSON-encoded string).
        payload: String,
        /// The Unix timestamp (in seconds) of when the message was created.
        timestamp: i64,
    },
}
