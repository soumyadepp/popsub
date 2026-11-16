//! Standalone example of one PopSub session over WebSocket.
//!
//! It connects to `ws://127.0.0.1:8080` and walks through the session in six
//! printed steps: log in as `admin`, authenticate with the returned JWT,
//! subscribe to `chat`, publish a QoS 1 message to `chat`, wait for a
//! delivered message and acknowledge it, then unsubscribe. A self-registered
//! user (when `server.allow_registration` is on) would send a register frame
//! before logging in; this example doesn't.
//!
//! The frames here use an externally tagged shape such as
//! `{"Login": {"username": ..., "password": ...}}`, while `run_client` in the
//! binary sends the `type`-tagged shape such as `{"type": "login", ...}`.
//!
//! Run the server first with `cargo run --bin popsub server`, then this
//! example with `cargo run --example simple_client`.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

/// Runs the example session against a server on `127.0.0.1:8080`.
///
/// After login it reads one reply and looks for the token under
/// `LoginResponse.token`; without one it prints an error and returns. After
/// publishing it reads frames until one has a `Message` key, sends an `Ack`
/// for its `message_id` if present, and stops waiting, or until the server
/// closes the connection. Panics if the connection can't be opened, a frame
/// can't be sent, or the login reply is not valid JSON.
#[tokio::main]
async fn main() {
    let url = Url::parse("ws://127.0.0.1:8080").unwrap();
    let (mut ws_stream, _response) = connect_async(url.as_str())
        .await
        .expect("Failed to connect");

    println!("Connected to PopSub server!\n");

    println!("Step 1: Logging in...");
    let login = json!({"Login": {"username": "admin", "password": "password"}});
    ws_stream
        .send(WsMessage::Text(login.to_string().into()))
        .await
        .unwrap();

    let token = if let Some(Ok(WsMessage::Text(msg))) = ws_stream.next().await {
        println!("Login response: {msg}");
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        v.get("LoginResponse")
            .and_then(|lr| lr.get("token"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    let Some(token) = token else {
        eprintln!("Failed to get token from login response");
        return;
    };

    println!("\nStep 2: Authenticating with token...");
    let auth = json!({"Auth": {"token": token}});
    ws_stream
        .send(WsMessage::Text(auth.to_string().into()))
        .await
        .unwrap();

    if let Some(Ok(WsMessage::Text(auth_resp))) = ws_stream.next().await {
        println!("Auth response: {auth_resp}");
    }

    println!("\nStep 3: Subscribing to 'chat' topic...");
    let subscribe = json!({"Subscribe": {"topic": "chat"}});
    ws_stream
        .send(WsMessage::Text(subscribe.to_string().into()))
        .await
        .unwrap();

    println!("\nStep 4: Publishing message to 'chat' topic...");
    let publish = json!({
        "Publish": {
            "topic": "chat",
            "payload": "Hello from PopSub example!",
            "qos": 1
        }
    });
    ws_stream
        .send(WsMessage::Text(publish.to_string().into()))
        .await
        .unwrap();

    println!("\nStep 5: Waiting for messages...");
    while let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            WsMessage::Text(text) => {
                println!("Received: {text}");
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                    && v.get("Message").is_some()
                {
                    if let Some(msg_id) = v
                        .get("Message")
                        .and_then(|m| m.get("message_id"))
                        .and_then(|id| id.as_str())
                    {
                        let ack = json!({"Ack": {"message_id": msg_id}});
                        ws_stream
                            .send(WsMessage::Text(ack.to_string().into()))
                            .await
                            .unwrap();
                        println!("Sent ACK for message: {msg_id}");
                    }
                    break;
                }
            }
            WsMessage::Close(_) => {
                println!("Connection closed");
                break;
            }
            _ => {}
        }
    }

    println!("\nStep 6: Unsubscribing from 'chat' topic...");
    let unsubscribe = json!({"Unsubscribe": {"topic": "chat"}});
    ws_stream
        .send(WsMessage::Text(unsubscribe.to_string().into()))
        .await
        .unwrap();

    println!("\nDone!");
}
