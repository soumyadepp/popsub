use serde::{Deserialize, Serialize};

/// Represents a published message in the Pub/Sub system.
///
/// A message consists of a topic identifier, the payload content,
/// and a timestamp indicating when it was published.
///
/// This structure is used for serialization to and from JSON
/// for communication over WebSocket and for persistence.
///
/// # Fields
///
/// - `topic` - The name of the topic this message belongs to.
/// - `payload` - The actual message content, usually a JSON-encoded string.
/// - `timestamp` - The Unix timestamp (in seconds) representing when the message was created.
///
/// # Example
///
/// ```rust
/// let msg = Message {
///     topic: "sensor_updates".to_string(),
///     payload: "{\"temp\":25}".to_string(),
///     timestamp: 1_725_000_000,
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub topic: String,
    pub payload: String,
    pub timestamp: i64,
}
