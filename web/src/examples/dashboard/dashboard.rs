use std::collections::BTreeMap;

use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;
    let mut readings = client
        .subscribe("sensors/+/temp", StartFrom::Last(1))
        .await?;

    let mut latest = BTreeMap::new();
    while let Some(message) = readings.next().await {
        let room = message
            .topic
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        latest.insert(room, message.payload);
        let rooms: Vec<String> = latest
            .iter()
            .map(|(room, value)| format!("{room} {value}"))
            .collect();
        println!("{}", rooms.join("  "));
    }
    Ok(())
}
