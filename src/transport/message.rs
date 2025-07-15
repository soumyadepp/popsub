use serde::Deserialize;

/// Represents a message sent by a client
/// This struct is used to encapsulate the data sent by clients
/// It includes the type of message (subscribe, unsubscribe, publish), the topic,
/// the payload (if applicable), and a timestamp for the message
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "subscribe")]
    Subscribe { topic: String },

    #[serde(rename = "unsubscribe")]
    Unsubscribe { topic: String },

    #[serde(rename = "publish")]
    Publish {
        topic: String,
        payload: String,
        timestamp: u64,
    },
}
