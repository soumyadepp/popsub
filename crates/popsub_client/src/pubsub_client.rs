//! `Client`, the server-side state kept for one connected socket.
//!
//! The broker and transport push outgoing frames into the client's channel,
//! and the transport's send loop writes them to the socket. The transport
//! sets `authenticated` and `username` once a JWT has been validated.

use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

/// The server's record of one connected socket.
///
/// The channel behind `sender` is unbounded, so frames for a slow reader
/// queue in memory without limit.
#[derive(Debug)]
pub struct Client {
    /// UUID for this connection; a reconnect gets a new one.
    pub id: String,
    /// Sending side of the channel the transport drains into the socket.
    pub sender: UnboundedSender<WsMessage>,
    /// Whether the connection has presented a valid JWT.
    pub authenticated: bool,
    /// The username of the authenticated user (populated after auth).
    pub username: Option<String>,
}

impl Client {
    /// Creates an unauthenticated client with a fresh UUID as its id.
    ///
    /// Args:
    ///     sender: Sending side of the channel whose receiver writes to this
    ///         client's socket.
    ///
    /// Returns:
    ///     A client with no username and `authenticated` set to false.
    pub fn new(sender: UnboundedSender<WsMessage>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sender,
            authenticated: false,
            username: None,
        }
    }
}
