use popsub_sdk::{
    Access, Client, ClientConfig, Credentials, Error, ErrorCode, RoleSpec, TopicGrant,
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let username = std::env::args()
        .nth(1)
        .expect("usage: provision <username>");
    let config = ClientConfig::new(
        "ws://127.0.0.1:8080",
        Credentials::password("admin", "password"),
    );
    let client = Client::connect(config).await?;

    let inbox = format!("inbox/{username}/#");
    let role = RoleSpec::user(vec![TopicGrant::new(&inbox, Access::Subscribe)]);
    let created = client
        .admin()
        .create_user(&username, format!("{username}-password"), role)
        .await;
    client.close();
    match created {
        Ok(_) => println!("created {username}, may read {inbox}"),
        Err(Error::Server {
            code: ErrorCode::UserExists,
            ..
        }) => println!("{username} already exists"),
        Err(error) => return Err(error),
    }
    Ok(())
}
