use serde::{Deserialize, Serialize};

/// Represents a message in the broker system
/// Contains a topic, payload, and timestamp
/// The topic is the channel to which the message belongs
/// The payload is the content of the message
/// The timestamp indicates when the message was created
/// This struct is used for serialization and deserialization of messages
/// to and from JSON format, allowing for easy communication between clients and the broker
/// It is also used to publish messages to subscribers of a topic
/// The `Message` struct is essential for the pub/sub functionality of the broker
/// It allows clients to send and receive messages in a structured format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub topic: String,
    pub payload: String,
    pub timestamp: i64,
}
