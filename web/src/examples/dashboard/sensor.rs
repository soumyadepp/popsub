use std::time::Duration;

use popsub_sdk::{Client, ClientConfig, Credentials};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let room = std::env::args().nth(1).expect("usage: sensor <room>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;

    let topic = format!("sensors/{room}/temp");
    let mut temperature = 20.0;
    loop {
        client.publish(&topic, format!("{temperature:.1}"))?;
        temperature += 0.1;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
