//! Raw WebSocket connections to the server under test.
//!
//! The load generator speaks the protocol directly instead of going through `popsub_sdk`, so the
//! client library's reconnection, deduplication and buffering are not part of what is measured.

use futures_util::{SinkExt, StreamExt};
use popsub_protocol::{
    Access, ClientFrame, ClientMessage, ErrorCode, PROTOCOL_VERSION, RequestId, RoleSpec,
    ServerFrame, ServerMessage, StartFrom, TopicGrant,
};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};

/// One client connection.
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The password every benchmark account is created with.
pub const BENCH_PASSWORD: &str = "bench-password";

/// Opens a WebSocket to the server with Nagle's algorithm off, so small frames are sent at once.
///
/// Args:
///     url: The server's WebSocket URL.
///
/// Returns:
///     The open socket.
///
/// Raises:
///     String: When the connection or the WebSocket handshake fails. A failure that mentions too
///         many open files means the file descriptor limit is too low for the scenario.
pub async fn connect(url: &str) -> Result<Socket, String> {
    let (socket, _) = connect_async_with_config(url, None, true)
        .await
        .map_err(|e| format!("connecting to {url} failed: {e}"))?;
    Ok(socket)
}

/// Sends one request and waits for the reply that carries its id.
///
/// Frames without that id, such as deliveries, are discarded.
///
/// Args:
///     socket: The connection to use.
///     id: The request id, unique on this connection.
///     message: The request.
///
/// Returns:
///     The reply.
///
/// Raises:
///     String: When the socket fails or closes, or the server answers with an `error` frame.
pub async fn request(
    socket: &mut Socket,
    id: u64,
    message: ClientMessage,
) -> Result<ServerMessage, String> {
    let kind = message.kind();
    let frame = ClientFrame::with_id(RequestId::Number(id), message);
    let json = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
    socket
        .send(Message::text(json))
        .await
        .map_err(|e| format!("sending {kind} failed: {e}"))?;
    while let Some(next) = socket.next().await {
        let Message::Text(text) =
            next.map_err(|e| format!("reading the {kind} reply failed: {e}"))?
        else {
            continue;
        };
        let Ok(reply) = serde_json::from_str::<ServerFrame>(text.as_str()) else {
            continue;
        };
        if reply.id != Some(RequestId::Number(id)) {
            continue;
        }
        return match reply.message {
            ServerMessage::Error { code, message, .. } => {
                Err(format!("{kind} was refused: {} ({message})", code.as_str()))
            }
            other => Ok(other),
        };
    }
    Err(format!("the connection closed before the {kind} reply"))
}

/// Logs in on a fresh connection and returns the token.
///
/// Args:
///     url: The server's WebSocket URL.
///     username: The account.
///     password: Its password.
///
/// Returns:
///     A token any number of connections can authenticate with.
///
/// Raises:
///     String: When connecting or the login fails.
pub async fn login(url: &str, username: &str, password: &str) -> Result<String, String> {
    let mut socket = connect(url).await?;
    let reply = request(
        &mut socket,
        1,
        ClientMessage::Login {
            username: username.to_string(),
            password: password.to_string(),
        },
    )
    .await?;
    match reply {
        ServerMessage::LoggedIn { token } => Ok(token),
        other => Err(format!("unexpected login reply: {other:?}")),
    }
}

/// Opens a connection and authenticates it with a token, without a durable session.
///
/// Args:
///     url: The server's WebSocket URL.
///     token: A token from `login`.
///
/// Returns:
///     The authenticated socket.
///
/// Raises:
///     String: When connecting or authenticating fails.
pub async fn open(url: &str, token: &str) -> Result<Socket, String> {
    let mut socket = connect(url).await?;
    request(
        &mut socket,
        1,
        ClientMessage::Auth {
            token: token.to_string(),
            protocol: Some(PROTOCOL_VERSION),
            session_id: None,
            clean_start: false,
            hold_resumed: false,
        },
    )
    .await?;
    Ok(socket)
}

/// Subscribes a connection to a topic or pattern and waits for the server to accept it.
///
/// Args:
///     socket: An authenticated connection.
///     topic: The topic or pattern to subscribe to.
///     from: Where in each topic's log to start.
///     group: A shared subscription group to join, if any.
///
/// Raises:
///     String: When the request fails or is refused.
pub async fn subscribe(
    socket: &mut Socket,
    topic: &str,
    from: StartFrom,
    group: Option<&str>,
) -> Result<(), String> {
    request(
        socket,
        2,
        ClientMessage::Subscribe {
            topic: topic.to_string(),
            from,
            group: group.map(str::to_string),
        },
    )
    .await?;
    Ok(())
}

/// Creates an account that may use every topic under `bench/` in one direction.
///
/// An account that already exists is left as it is.
///
/// Args:
///     admin: A connection authenticated as an admin.
///     id: The request id to use on that connection.
///     username: The account to create, with `BENCH_PASSWORD`.
///     access: Whether it publishes or subscribes.
///
/// Raises:
///     String: When the request fails for any reason other than the account existing.
pub async fn create_account(
    admin: &mut Socket,
    id: u64,
    username: &str,
    access: Access,
) -> Result<(), String> {
    let created = request(
        admin,
        id,
        ClientMessage::CreateUser {
            username: username.to_string(),
            password: BENCH_PASSWORD.to_string(),
            role: RoleSpec::user(vec![TopicGrant::new("bench/#", access)]),
        },
    )
    .await;
    match created {
        Ok(_) => Ok(()),
        Err(e) if e.contains(ErrorCode::UserExists.as_str()) => Ok(()),
        Err(e) => Err(e),
    }
}
