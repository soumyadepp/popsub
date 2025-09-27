use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

/// Represents a connected WebSocket client in the Pub/Sub system.
///
/// Each client is uniquely identified by an `id` and has an associated
/// `UnboundedSender` for sending WebSocket messages back to the client.
/// This struct encapsulates the necessary information to manage a client's
/// connection and communication within the Pub/Sub broker.
#[derive(Debug)]
pub struct Client {
    /// A unique identifier for this client connection.
    /// This is typically a UUID generated upon connection.
    pub id: String,

    /// An unbounded MPSC (Multi-Producer, Single-Consumer) sender channel
    /// used to send `WsMessage`s to this specific client's WebSocket connection.
    pub sender: UnboundedSender<WsMessage>,

    /// A flag to indicate if the client is authenticated.
    pub authenticated: bool,
}

impl Client {
    /// Creates a new `Client` instance with a randomly generated UUID and the provided sender.
    ///
    /// # Arguments
    ///
    /// * `sender` - An `UnboundedSender<WsMessage>` used to send messages to this client.
    ///
    /// # Returns
    ///
    /// A new `Client` instance.
    pub fn new(sender: UnboundedSender<WsMessage>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sender,
            authenticated: false,
        }
    }
}
