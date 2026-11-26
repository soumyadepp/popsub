use popsub_sdk::{Client, ClientConfig, Credentials, Error, StartFrom};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let username = std::env::args().nth(1).expect("usage: inbox <username>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password(&username, format!("{username}-password")),
    );
    let client = Client::connect(config).await?;

    let notes = format!("inbox/{username}/notes");
    if let Err(Error::Server { code, .. }) = client.publish_confirmed(notes, "hello").await {
        println!("publish refused: {}", code.as_str());
    }

    let mut inbox = client
        .subscribe(format!("inbox/{username}/#"), StartFrom::Seq(1))
        .await?;
    while let Some(message) = inbox.next().await {
        println!("{}: {}", message.topic, message.payload);
    }
    Ok(())
}
