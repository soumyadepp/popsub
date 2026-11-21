//! Publishes one message and waits until the server has stored it.
//!
//! Example:
//!
//! ```text
//! cargo run -p popsub_sdk --example publish -- orders/rust 'hello from Rust'
//! ```
//!
//! `POPSUB_URL` defaults to `ws://127.0.0.1:8080`, `POPSUB_USERNAME` to `alice` and
//! `POPSUB_USER_PASSWORD` to `alice-password`. The topic defaults to `orders/rust` and the
//! message to `hello from Rust`.

use popsub_sdk::{Client, ClientConfig, Credentials};

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

/// Connects, publishes the message with confirmation and prints where it was stored.
///
/// Raises:
///     popsub_sdk::Error: When connecting fails or the server refuses the publish.
#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let mut args = std::env::args().skip(1);
    let topic = args.next().unwrap_or_else(|| "orders/rust".to_string());
    let payload = args.next().unwrap_or_else(|| "hello from Rust".to_string());
    let config = ClientConfig::new(
        setting("POPSUB_URL", "ws://127.0.0.1:8080"),
        Credentials::password(
            setting("POPSUB_USERNAME", "alice"),
            setting("POPSUB_USER_PASSWORD", "alice-password"),
        ),
    );
    let client = Client::connect(config).await?;
    let stored = client.publish_confirmed(&topic, payload).await?;
    println!(
        "stored on {topic} as seq {} ({})",
        stored.seq, stored.message_id
    );
    Ok(())
}
