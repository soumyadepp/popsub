use popsub_sdk::{Client, ClientConfig, Credentials};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;

    for n in 1..=6 {
        let payload = format!("job-{n}");
        client
            .publish_confirmed("jobs/resize", payload.as_str())
            .await?;
        println!("queued {payload}");
    }
    client.close();
    Ok(())
}
