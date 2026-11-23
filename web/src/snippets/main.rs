use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    )
    .session("orders-worker");
    let client = Client::connect(config).await?;

    let mut messages = client.subscribe("orders/#", StartFrom::Seq(1)).await?;
    client.publish_confirmed("orders/42", "order 42 shipped").await?;

    while let Some(message) = messages.next().await {
        println!("{} #{}: {}", message.topic, message.seq, message.payload);
    }
    Ok(())
}
