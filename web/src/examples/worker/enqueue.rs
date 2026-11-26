use popsub_sdk::{Client, ClientConfig, Credentials};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let count: u32 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .expect("usage: enqueue <count>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;

    for k in 1..=count {
        let topic = format!("invoices/{k}");
        let stored = client
            .publish_confirmed(&topic, format!("invoice {k}"))
            .await?;
        println!("queued {topic} as #{}", stored.seq);
    }
    client.close();
    Ok(())
}
