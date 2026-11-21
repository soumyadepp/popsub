//! Subscribes to a topic pattern with a durable session and prints every message.
//!
//! Stop it and run it again: messages published while it was away arrive once
//! it is back, each once, because the session keeps its place.
//!
//! Example:
//!
//! ```text
//! cargo run -p popsub_sdk --example subscribe -- 'orders/#'
//! ```
//!
//! `POPSUB_URL` defaults to `ws://127.0.0.1:8080`, `POPSUB_USERNAME` to `alice` and
//! `POPSUB_USER_PASSWORD` to `alice-password`; the pattern defaults to `orders/#`.

use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

/// Reads an environment variable, or a default when it is unset.
///
/// Args:
///     name: The variable.
///     default: The value to use when it is unset.
///
/// Returns:
///     The variable's value, or `default`.
fn setting(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Connects with a durable session, subscribes and prints each message until the client stops.
///
/// Raises:
///     popsub_sdk::Error: When connecting or subscribing fails.
#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let pattern = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "orders/#".to_string());
    let config = ClientConfig::new(
        setting("POPSUB_URL", "ws://127.0.0.1:8080"),
        Credentials::password(
            setting("POPSUB_USERNAME", "alice"),
            setting("POPSUB_USER_PASSWORD", "alice-password"),
        ),
    )
    .session("examples-subscriber");
    let client = Client::connect(config).await?;
    let mut messages = client.subscribe(&pattern, StartFrom::Resume).await?;
    println!("subscribed to {pattern}; waiting for messages (Ctrl+C to stop)");
    while let Some(message) = messages.next().await {
        println!("{} #{}: {}", message.topic, message.seq, message.payload);
    }
    Ok(())
}
