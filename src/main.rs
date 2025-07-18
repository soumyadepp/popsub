#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
mod broker;
mod client;
mod config;
mod persistence;
mod transport;

use broker::Broker;
use broker::message::Message;
use std::sync::{Arc, Mutex};
use transport::websocket::start_websocket_server;

use crate::config::load_config;

#[tokio::main]
async fn main() {
    let config = load_config().expect("Failed to load configuration");
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let broker = Arc::new(Mutex::new(Broker::new()));
    start_websocket_server(&addr, broker).await;
}
