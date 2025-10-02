use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

#[tokio::main]
async fn main() {
    let url = Url::parse("ws://127.0.0.1:8080").unwrap();
    let (mut ws_stream, _response) = connect_async(url.as_str())
        .await
        .expect("Failed to connect");

    // 1. Login
    let login = json!({ "type": "login", "username": "admin", "password": "password" });
    ws_stream
        .send(WsMessage::Text(login.to_string().into()))
        .await
        .unwrap();

    // 2. Read LoginResponse
    if let Some(Ok(WsMessage::Text(msg))) = ws_stream.next().await {
        println!("Login response: {msg}");
        // Extract token
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        if let Some(token) = v.get("token").and_then(|t| t.as_str()) {
            // 3. Auth
            let auth = json!({ "type": "auth", "token": token });
            ws_stream
                .send(WsMessage::Text(auth.to_string().into()))
                .await
                .unwrap();
            if let Some(Ok(WsMessage::Text(auth_resp))) = ws_stream.next().await {
                println!("Auth response: {auth_resp}");
            }

            // 4. Subscribe
            let subscribe = json!({ "type": "subscribe", "topic": "chat" });
            ws_stream
                .send(WsMessage::Text(subscribe.to_string().into()))
                .await
                .unwrap();

            // 5. Publish
            let publish = json!({ "type": "publish", "topic": "chat", "payload": "Hello from example", "qos": 0 });
            ws_stream
                .send(WsMessage::Text(publish.to_string().into()))
                .await
                .unwrap();

            // Read any incoming message
            if let Some(Ok(WsMessage::Text(incoming))) = ws_stream.next().await {
                println!("Incoming: {incoming}");
            }
        }
    }
}
