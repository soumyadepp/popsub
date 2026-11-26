use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let name = std::env::args().nth(1).expect("usage: chat <name>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;
    let mut lobby = client.subscribe("chat/lobby", StartFrom::Last(20)).await?;

    let publisher = client.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.is_empty() {
                let _ = publisher.publish("chat/lobby", format!("{name}: {line}"));
            }
        }
        publisher.close();
    });

    while let Some(message) = lobby.next().await {
        println!("{}", message.payload);
    }
    Ok(())
}
