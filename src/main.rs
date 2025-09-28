mod broker;
mod client;
mod config;
mod persistence;
mod transport;

use broker::engine::Broker;
use config::load_config;
use std::sync::{Arc, Mutex};
use tracing::{error, info};
use transport::websocket::start_websocket_server;

/// The main entry point for the PopSub server.
///
/// This function initializes the logger, loads the configuration, starts the
/// WebSocket server, and listens for a shutdown signal (Ctrl+C) to gracefully
/// terminate the application.
#[tokio::main]
async fn main() {
    // Initialize the tracing subscriber for logging
    tracing_subscriber::fmt::init();

    // Load configuration with robust error handling
    let config = match load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            return;
        }
    };

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let broker = Arc::new(Mutex::new(Broker::new()));

    // Spawn the retry loop for QoS 1 messages
    tokio::spawn(Broker::start_retry_loop(broker.clone()));

    // Run the server and listen for a shutdown signal
    tokio::select! {
        _ = start_websocket_server(addr, broker, config.clone()) => {
            error!("WebSocket server exited unexpectedly.");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received. Exiting gracefully.");
        }
    }
}
