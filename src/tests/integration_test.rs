#[tokio::test]
async fn integration_pubsub_end_to_end() {
    use tokio_tungstenite::connect_async;
    use url::Url;
    use serde_json::json;
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;

    use crate::broker::Broker;
    use crate::transport::message::ClientMessage;

    let broker = Arc::new(Mutex::new(Broker::new()));
    let addr = "127.0.0.1:9001";

    let server_broker = broker.clone();
    tokio::spawn(async move {
        crate::server::start_websocket_server(addr, server_broker).await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    let url = Url::parse(&format!("ws://{}", addr)).unwrap();
    let (mut ws_a, _) = connect_async(url.clone()).await.expect("client A connect");
    let (mut ws_b, _) = connect_async(url.clone()).await.expect("client B connect");

    let sub_msg = json!({
        "type": "subscribe",
        "topic": "test"
    })
    .to_string();
    ws_b.send(WsMessage::Text(sub_msg)).await.unwrap();

    let pub_msg = json!({
        "type": "publish",
        "topic": "test",
        "payload": "hello world"
    })
    .to_string();
    ws_a.send(WsMessage::Text(pub_msg)).await.unwrap();

    if let Some(Ok(WsMessage::Text(msg))) = ws_b.next().await {
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["topic"], "test");
        assert_eq!(parsed["payload"], "hello world");
    } else {
        panic!("Client B did not receive the published message");
    }
}
