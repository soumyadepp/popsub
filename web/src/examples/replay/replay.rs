use std::time::{SystemTime, UNIX_EPOCH};

use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let minutes: i64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .expect("usage: replay <minutes>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;

    let now_ms = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    let since_ms = now_ms() - minutes * 60_000;
    let mut audit = client
        .subscribe("audit/#", StartFrom::SinceMs(since_ms))
        .await?;
    while let Some(message) = audit.next().await {
        let seconds = (now_ms() - message.published_at_ms).div_euclid(1000);
        println!("{seconds} s ago  {}  {}", message.topic, message.payload);
    }
    Ok(())
}
