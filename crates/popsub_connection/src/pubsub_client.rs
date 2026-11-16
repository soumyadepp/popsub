//! `Client`, the server-side state kept for one connected socket.
//!
//! The broker and transport push outgoing frames into the client's channel,
//! and the transport's send loop writes them to the socket. The transport
//! sets `authenticated` and `username` once a JWT has been validated, and
//! `session` when the client named a durable session in its `auth`.

use uuid::Uuid;

use crate::outbox::Outbox;

/// The server's record of one connected socket.
///
/// Frames reach the socket through `sender`, an `Outbox` with a byte budget,
/// so a slow reader can't make the server queue without limit.
#[derive(Debug)]
pub struct Client {
    /// UUID for this connection; a reconnect gets a new one.
    pub id: String,
    /// The connection's outgoing queue, drained into the socket by the transport.
    pub sender: Outbox,
    /// Whether the connection has presented a valid JWT.
    pub authenticated: bool,
    /// The username of the authenticated user (populated after auth).
    pub username: Option<String>,
    /// The storage key of the durable session this connection holds, or
    /// `None` for an ephemeral connection. Built from the username and the
    /// client's `session_id`, so one user can't claim another's session.
    pub session: Option<String>,
}

impl Client {
    /// Creates an unauthenticated client with a fresh UUID as its id.
    ///
    /// Args:
    ///     sender: The outgoing queue whose receiver writes to this client's socket.
    ///
    /// Returns:
    ///     A client with no username or session and `authenticated` set to false.
    pub fn new(sender: Outbox) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sender,
            authenticated: false,
            username: None,
            session: None,
        }
    }
}
