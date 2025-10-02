use super::pubsub_client::Client;
use tokio::sync::mpsc;
use tungstenite::protocol::Message as WsMessage;

#[test]
fn test_client_new() {
    let (tx, _) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    assert!(!client.id.is_empty());
}
