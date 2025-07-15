use serde::Deserialize;

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
