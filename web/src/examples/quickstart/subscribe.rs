use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;
    let mut greetings = client.subscribe("greetings", StartFrom::default()).await?;
    println!("waiting for messages on greetings");

    while let Some(message) = greetings.next().await {
        println!("{} #{}: {}", message.topic, message.seq, message.payload);
    }
    Ok(())
}
