use std::time::Duration;

use popsub_sdk::{AckMode, Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let name = std::env::args().nth(1).expect("usage: worker <name>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    )
    .ack_mode(AckMode::Manual);
    let client = Client::connect(config).await?;
    let mut jobs = client
        .subscribe_shared("jobs/resize", "resizers", StartFrom::New)
        .await?;

    while let Some(job) = jobs.next().await {
        println!("{name} took {}", job.payload);
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("{name} finished {}", job.payload);
        job.ack();
    }
    Ok(())
}
