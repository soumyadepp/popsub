use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;

/// Represents a client in the WebSocket server
/// Contains a unique identifier for the client and a sender channel
/// This struct is used to manage client connections and send messages to clients
#[derive(Debug)]
pub struct Client {
    pub id: String,
    pub sender: UnboundedSender<WsMessage>,
}
