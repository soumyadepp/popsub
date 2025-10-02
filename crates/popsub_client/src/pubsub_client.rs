//! Client representation
//!
//! `Client` models a connected client and holds the sending side of a
//! per-client channel used by the broker/transport to push messages. The
//! `authenticated` flag is updated by the transport after successful JWT
//! validation.

use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

#[derive(Debug)]
pub struct Client {
    pub id: String,
    pub sender: UnboundedSender<WsMessage>,
    pub authenticated: bool,
}

impl Client {
    /// Create a new client with a sender channel. The `id` is a UUID used
    /// to identify the client across broker operations.
    pub fn new(sender: UnboundedSender<WsMessage>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sender,
            authenticated: false,
        }
    }
}
