//! CLI for PopSub
//!
//! Subcommands:
//! - `server`: run the WebSocket server
//! - `client`: run a simple example client (useful for smoke tests)

use clap::Parser;
use popsub_broker::engine::Broker;
use popsub_config::load_config;
use popsub_transport::websocket::start_websocket_server;
use std::sync::{Arc, Mutex};
use tracing::{error, info};

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

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let broker = Arc::new(Mutex::new(Broker::new()));

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

async fn run_client(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Use the example client's logic here so `cargo run -- client` behaves
    // like the former example.
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let (mut ws_stream, _response) = connect_async(url).await?;

    // 1. Login
    let login = json!({ "type": "login", "username": "admin", "password": "password" });
    ws_stream
        .send(WsMessage::Text(login.to_string().into()))
        .await?;

    // 2. Read LoginResponse
    if let Some(Ok(WsMessage::Text(msg))) = ws_stream.next().await {
        println!("Login response: {msg}");
        // Extract token
        let v: serde_json::Value = serde_json::from_str(&msg)?;
        if let Some(token) = v.get("token").and_then(|t| t.as_str()) {
            // 3. Auth
            let auth = json!({ "type": "auth", "token": token });
            ws_stream
                .send(WsMessage::Text(auth.to_string().into()))
                .await?;
            if let Some(Ok(WsMessage::Text(auth_resp))) = ws_stream.next().await {
                println!("Auth response: {auth_resp}");
            }

            // 4. Subscribe
            let subscribe = json!({ "type": "subscribe", "topic": "chat" });
            ws_stream
                .send(WsMessage::Text(subscribe.to_string().into()))
                .await?;

            // 5. Publish
            let publish = json!({ "type": "publish", "topic": "chat", "payload": "Hello from example", "qos": 0 });
            ws_stream
                .send(WsMessage::Text(publish.to_string().into()))
                .await?;

            // Read any incoming message
            if let Some(Ok(WsMessage::Text(incoming))) = ws_stream.next().await {
                println!("Incoming: {incoming}");
            }
        }
    }

    Ok(())
}
