use popsub_sdk::{Client, ClientConfig, Credentials, Error};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let (Some(topic), Some(text)) = (args.next(), args.next()) else {
        panic!("usage: publish <topic> <text>");
    };
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("alice", "alice-password"),
    );
    let client = Client::connect(config).await?;

    let result = client.publish_confirmed(&topic, text).await;
    client.close();
    match result {
        Ok(stored) => println!("stored on {topic} as #{}", stored.seq),
        Err(Error::Server { code, .. }) => {
            eprintln!("refused: {}", code.as_str());
            std::process::exit(1);
        }
        Err(error) => return Err(error),
    }
    Ok(())
}
