use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;

/// Represents a connected WebSocket client in the Pub/Sub system.
///
/// Each client is uniquely identified by an `id` and has a channel (`sender`)
/// for sending messages to the client over WebSocket.
#[derive(Debug)]
pub struct Client {
    /// Unique identifier for the client (e.g. UUID or connection ID).
    pub id: String,

    /// Channel to send WebSocket messages to the client.
    pub sender: UnboundedSender<WsMessage>,
}
