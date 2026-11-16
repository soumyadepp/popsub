//! Command-line entry point for PopSub.
//!
//! The `server` subcommand loads settings, builds the broker, spawns its QoS 1
//! retry loop and runs the WebSocket server until Ctrl+C. The `client`
//! subcommand runs a short demo session (login, auth, subscribe, publish)
//! against a running server, for smoke tests. Everything here is wiring; the
//! protocol, routing and storage live in the transport, broker and
//! persistence crates.

use std::process::ExitCode;

use clap::Parser;
use popsub_broker::{Broker, SharedBroker};
use popsub_config::load_config;
use popsub_persistence::Persistence;
use popsub_transport::{ClientMessage, Server, ServerMessage};
use tracing::{error, info, warn};

/// Where the server keeps its message history, relative to the working directory.
const DATA_DIR: &str = "pubsub_db";

/// The most messages kept per topic for replay to new subscribers.
const MAX_MESSAGES_PER_TOPIC: usize = 1000;

/// Runs the PopSub WebSocket broker or a demo client against it.
#[derive(Parser)]
#[command(name = "popsub")]
enum Command {
    /// Start the WebSocket server
    Server,
    /// Log in, subscribe to a topic, publish to it and print what comes back
    Client {
        /// WebSocket URL of the server
        #[arg(long, default_value = "ws://127.0.0.1:8080")]
        url: String,
        /// Username to log in as
        #[arg(long, default_value = "admin")]
        username: String,
        /// Password to log in with
        #[arg(long, default_value = "password")]
        password: String,
        /// Topic to subscribe and publish to
        #[arg(long, default_value = "chat")]
        topic: String,
    },
}

/// Starts logging at a fixed `info` level, parses the command line and runs
/// the chosen subcommand.
///
/// Returns:
///     Success when the subcommand finishes cleanly, failure (after logging
///     the error) when it fails, so scripts and process supervisors can tell.
#[tokio::main]
async fn main() -> ExitCode {
    popsub_utils::logging::init("info");

    let result = match Command::parse() {
        Command::Server => run_server().await,
        Command::Client {
            url,
            username,
            password,
            topic,
        } => run_client(&url, &username, &password, &topic).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Loads settings and runs the broker and WebSocket server until Ctrl+C.
///
/// If the JWT secret or admin password still has a published default value,
/// the server refuses to start on a non-loopback host and only logs a warning
/// for each such setting on `localhost` or a loopback address. Otherwise it
/// opens the message store in `DATA_DIR` (keeping messages for
/// `broker.message_ttl_secs`), builds the broker, spawns the QoS 1 retry
/// loop, and serves on `host:port` until Ctrl+C arrives.
///
/// Raises:
///     ConfigError::FileParse: When `config/default.toml` exists but can't be parsed.
///     ConfigError::Type: When a setting from the file or the environment has the wrong type.
///     String: When published default secrets are set and the host is not a loopback address.
///     String: When the message store in `DATA_DIR` can't be opened, usually
///         because another server running from the same directory holds its lock.
///     String: When `host:port` can't be bound, with the underlying I/O error.
async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let insecure = config.insecure_defaults();
    if !insecure.is_empty() {
        if !config.server.is_loopback() {
            return Err(format!(
                "refusing to listen on {} with published defaults ({}). Set them in config/default.toml, or with SERVER__JWT_SECRET and SERVER_PASSWORD.",
                config.server.host,
                insecure.join(", ")
            )
            .into());
        }
        for problem in &insecure {
            warn!("{problem}. Fine for local development, never for a public address.");
        }
    }
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let ttl = i64::try_from(config.broker.message_ttl_secs).unwrap_or(i64::MAX);
    let persistence = Persistence::new(DATA_DIR, Some(ttl), Some(MAX_MESSAGES_PER_TOPIC))
        .map_err(|e| format!("can't open the message store in {DATA_DIR}: {e}. Is another PopSub server running from this directory?"))?;
    let broker = SharedBroker::new(Broker::new(persistence, config.broker.clone()));
    tokio::spawn(broker.clone().run_retry_loop());
    let server = Server::from_settings(broker, &config.server).await;

    tokio::select! {
        served = server.listen(&addr) => served.map_err(|e| format!("can't serve on {addr}: {e}"))?,
        _ = tokio::signal::ctrl_c() => info!("shutdown signal received"),
    }
    Ok(())
}

/// Runs a short session against a PopSub server and prints each reply.
///
/// Logs in, authenticates with the returned token, subscribes to `topic`,
/// publishes `Hello from popsub client` to it at QoS 0, and prints the
/// delivered message. Frames are built from the transport's own message
/// types, so the client always speaks the current protocol. The defaults
/// match the default server settings; pass `--username` and `--password` for
/// any other account.
///
/// Args:
///     url: WebSocket URL of the server, such as `ws://127.0.0.1:8080`.
///     username: The account to log in as.
///     password: The account's password.
///     topic: The topic to subscribe and publish to.
///
/// Raises:
///     tungstenite::Error: When the connection can't be opened or drops.
///     serde_json::Error: When a frame can't be encoded or decoded.
///     String: When login, auth or subscribe is refused, or the server closes
///         the connection before replying.
async fn run_client(
    url: &str,
    username: &str,
    password: &str,
    topic: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ws, _response) = tokio_tungstenite::connect_async(url).await?;

    let reply = request(
        &mut ws,
        &ClientMessage::Login {
            username: username.to_string(),
            password: password.to_string(),
        },
    )
    .await?;
    let ServerMessage::LoginResponse { token } = reply else {
        return Err(format!("login refused: {reply:?}").into());
    };
    println!("Logged in as {username}");

    let reply = request(&mut ws, &ClientMessage::Auth { token }).await?;
    if !matches!(reply, ServerMessage::Authenticated {}) {
        return Err(format!("auth refused: {reply:?}").into());
    }
    println!("Authenticated");

    send(
        &mut ws,
        &ClientMessage::Subscribe {
            topic: topic.to_string(),
        },
    )
    .await?;
    send(
        &mut ws,
        &ClientMessage::Publish {
            topic: topic.to_string(),
            payload: "Hello from popsub client".to_string(),
            message_id: None,
            qos: Some(0),
        },
    )
    .await?;
    let delivered = next_text(&mut ws).await?;
    if delivered.contains("\"type\":\"error\"") {
        return Err(format!("subscribe or publish refused: {delivered}").into());
    }
    println!("Received on {topic}: {delivered}");
    Ok(())
}

/// The client side of a WebSocket connection, as returned by `connect_async`.
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Sends one protocol frame.
///
/// Args:
///     ws: The connection to send on.
///     msg: The frame to send.
///
/// Raises:
///     serde_json::Error: When the frame can't be encoded.
///     tungstenite::Error: When the connection drops.
async fn send(ws: &mut Socket, msg: &ClientMessage) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::SinkExt;
    let text = serde_json::to_string(msg)?;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
        .await?;
    Ok(())
}

/// Waits for the next text frame, skipping pings and other control frames.
///
/// Args:
///     ws: The connection to read from.
///
/// Returns:
///     The frame's text.
///
/// Raises:
///     tungstenite::Error: When the connection drops.
///     String: When the server closes the connection first.
async fn next_text(ws: &mut Socket) -> Result<String, Box<dyn std::error::Error>> {
    use futures_util::StreamExt;
    while let Some(frame) = ws.next().await {
        if let tokio_tungstenite::tungstenite::Message::Text(text) = frame? {
            return Ok(text.to_string());
        }
    }
    Err("the server closed the connection".into())
}

/// Sends a frame and decodes the server's reply.
///
/// Args:
///     ws: The connection to use.
///     msg: The frame to send.
///
/// Returns:
///     The server's reply.
///
/// Raises:
///     serde_json::Error: When a frame can't be encoded or decoded.
///     tungstenite::Error: When the connection drops.
///     String: When the server closes the connection before replying.
async fn request(
    ws: &mut Socket,
    msg: &ClientMessage,
) -> Result<ServerMessage, Box<dyn std::error::Error>> {
    send(ws, msg).await?;
    Ok(serde_json::from_str(&next_text(ws).await?)?)
}
