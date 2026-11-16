//! Command-line entry point for PopSub.
//!
//! The `server` subcommand loads settings, builds the broker, spawns its QoS 1
//! retry loop and runs the WebSocket server until Ctrl+C. The `client`
//! subcommand runs a short demo session (login, auth, subscribe, publish)
//! against a running server, for smoke tests. Everything here is wiring; the
//! protocol, routing and storage live in the transport, broker and
//! persistence crates.

use clap::Parser;
use popsub_broker::engine::Broker;
use popsub_config::load_config;
use popsub_transport::websocket::start_websocket_server;
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

/// Runs the PopSub WebSocket broker or a demo client against it.
#[derive(Parser)]
#[command(name = "popsub")]
enum Command {
    /// Start the WebSocket server
    Server,
    /// Run the example client (connects, login, auth, subscribe, publish)
    Client {
        /// WebSocket server URL to connect to (default: ws://127.0.0.1:8080)
        #[arg(long, default_value = "ws://127.0.0.1:8080")]
        url: String,
    },
}

/// Starts logging at a fixed `info` level, parses the command line and runs
/// the chosen subcommand.
///
/// A failing subcommand is logged with `error!` and the process still exits
/// with status 0.
#[tokio::main]
async fn main() {
    popsub_utils::logging::init("info");

    let cmd = Command::parse();

    match cmd {
        Command::Server => {
            if let Err(e) = run_server().await {
                error!("Server failed: {}", e);
            }
        }
        Command::Client { url } => {
            if let Err(e) = run_client(&url).await {
                error!("Client failed: {}", e);
            }
        }
    }
}

/// Loads settings and runs the broker and WebSocket server until Ctrl+C.
///
/// If the JWT secret or admin password still has a published default value,
/// the server refuses to start on a non-loopback host and only logs a warning
/// for each such setting on `localhost` or a loopback address. Otherwise it
/// builds the broker from the broker settings, spawns the QoS 1 retry loop,
/// and serves on `host:port` until the server future finishes or Ctrl+C
/// arrives. Both of those end the function with `Ok`.
///
/// Returns:
///     Unit once the server stops or a shutdown signal is received.
///
/// Raises:
///     ConfigError::FileParse: When `config/default.toml` exists but can't be parsed.
///     ConfigError::Type: When a setting from the file or the environment has the wrong type.
///     String: When published default secrets are set and the host is not a loopback address.
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
    let broker = Arc::new(Mutex::new(Broker::new(config.broker.clone())));

    tokio::spawn(Broker::start_retry_loop(broker.clone()));

    tokio::select! {
        _ = start_websocket_server(addr, broker, config.clone()) => {
            error!("WebSocket server exited unexpectedly.");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received. Exiting gracefully.");
        }
    }

    Ok(())
}

/// Runs a short demo session against a PopSub server and prints the replies.
///
/// Logs in as `admin` with password `password` (these match only the default
/// settings), reads one reply, and if it carries a `token`, authenticates with
/// it, prints the auth reply, subscribes to `chat`, publishes `Hello from
/// example` to `chat` at QoS 0 and prints the next text frame. If the login
/// reply has no token, it returns without doing anything else. The steps are
/// the same as `examples/simple_client.rs`, which uses a different frame
/// shape and also acknowledges and unsubscribes.
///
/// Args:
///     url: WebSocket URL of the server, such as `ws://127.0.0.1:8080`.
///
/// Returns:
///     Unit once the session has run.
///
/// Raises:
///     tungstenite::Error::Url: When `url` is not a valid WebSocket URL.
///     tungstenite::Error::Io: When the connection can't be opened.
///     tungstenite::Error::ConnectionClosed: When a frame is sent after the server closed the connection.
///     serde_json::Error: When the login reply is not valid JSON.
async fn run_client(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let (mut ws_stream, _response) = connect_async(url).await?;

    let login = json!({ "type": "login", "username": "admin", "password": "password" });
    ws_stream
        .send(WsMessage::Text(login.to_string().into()))
        .await?;

    if let Some(Ok(WsMessage::Text(msg))) = ws_stream.next().await {
        println!("Login response: {msg}");
        let v: serde_json::Value = serde_json::from_str(&msg)?;
        if let Some(token) = v.get("token").and_then(|t| t.as_str()) {
            let auth = json!({ "type": "auth", "token": token });
            ws_stream
                .send(WsMessage::Text(auth.to_string().into()))
                .await?;
            if let Some(Ok(WsMessage::Text(auth_resp))) = ws_stream.next().await {
                println!("Auth response: {auth_resp}");
            }

            let subscribe = json!({ "type": "subscribe", "topic": "chat" });
            ws_stream
                .send(WsMessage::Text(subscribe.to_string().into()))
                .await?;

            let publish = json!({ "type": "publish", "topic": "chat", "payload": "Hello from example", "qos": 0 });
            ws_stream
                .send(WsMessage::Text(publish.to_string().into()))
                .await?;

            if let Some(Ok(WsMessage::Text(incoming))) = ws_stream.next().await {
                println!("Incoming: {incoming}");
            }
        }
    }

    Ok(())
}
