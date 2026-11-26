use popsub_sdk::{AckMode, Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    )
    .session("invoice-worker")
    .ack_mode(AckMode::Manual);
    let client = Client::connect(config).await?;
    let mut invoices = client.subscribe("invoices/#", StartFrom::Seq(1)).await?;

    while let Some(message) = invoices.next().await {
        println!(
            "processing {} #{}: {}",
            message.topic, message.seq, message.payload
        );
        message.ack();
    }
    Ok(())
}
