use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "auth")]
    Auth { token: String },
    #[serde(rename = "login")]
    Login { username: String, password: String },
    #[serde(rename = "subscribe")]
    Subscribe { topic: String },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { topic: String },
    #[serde(rename = "publish")]
    Publish {
        topic: String,
        payload: String,
        message_id: Option<String>,
        qos: Option<u8>,
    },
    #[serde(rename = "ack")]
    Ack { message_id: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "login_response")]
    LoginResponse { token: String },
    #[serde(rename = "authenticated")]
    Authenticated {},
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "message")]
    Message {
        topic: String,
        payload: String,
        timestamp: i64,
        message_id: String,
        qos: u8,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
}
