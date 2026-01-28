//! Simple PopSub client example demonstrating the authentication flow.
//!
//! This example shows how to:
//! 1. Connect to the WebSocket server
//! 2. Register a new user (optional)
//! 3. Login with credentials
//! 4. Authenticate with JWT token
//! 5. Subscribe to topics and publish messages
//!
//! Run the server first: `cargo run --bin popsub server`
//! Then run this example: `cargo run --example simple_client`

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

    println!("Connected to PopSub server!\n");

    // ===========================================
    // Option A: Register a new user (if enabled)
    // ===========================================
    // Uncomment the following to register a new user:
    //
    // let register = json!({"Register": {"username": "alice", "password": "alice123"}});
    // ws_stream.send(WsMessage::Text(register.to_string().into())).await.unwrap();
    // if let Some(Ok(WsMessage::Text(msg))) = ws_stream.next().await {
    //     println!("Register response: {msg}");
    // }

    // ===========================================
    // Step 1: Login with credentials
    // ===========================================
    println!("Step 1: Logging in...");
    let login = json!({"Login": {"username": "admin", "password": "password"}});
    ws_stream
        .send(WsMessage::Text(login.to_string().into()))
        .await
        .unwrap();

    // Read LoginResponse and extract token
    let token = if let Some(Ok(WsMessage::Text(msg))) = ws_stream.next().await {
        println!("Login response: {msg}");
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        // Response format: {"LoginResponse": {"token": "..."}}
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

    // ===========================================
    // Step 2: Authenticate with JWT token
    // ===========================================
    println!("\nStep 2: Authenticating with token...");
    let auth = json!({"Auth": {"token": token}});
    ws_stream
        .send(WsMessage::Text(auth.to_string().into()))
        .await
        .unwrap();

    if let Some(Ok(WsMessage::Text(auth_resp))) = ws_stream.next().await {
        println!("Auth response: {auth_resp}");
    }

    // ===========================================
    // Step 3: Subscribe to a topic
    // ===========================================
    println!("\nStep 3: Subscribing to 'chat' topic...");
    let subscribe = json!({"Subscribe": {"topic": "chat"}});
    ws_stream
        .send(WsMessage::Text(subscribe.to_string().into()))
        .await
        .unwrap();

    // ===========================================
    // Step 4: Publish a message
    // ===========================================
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

    // ===========================================
    // Step 5: Receive messages
    // ===========================================
    println!("\nStep 5: Waiting for messages...");
    while let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            WsMessage::Text(text) => {
                println!("Received: {text}");
                // Parse and check if it's our published message
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                    && v.get("Message").is_some()
                {
                    // Acknowledge QoS 1 messages
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
                    break; // Exit after receiving our message
                }
            }
            WsMessage::Close(_) => {
                println!("Connection closed");
                break;
            }
            _ => {}
        }
    }

    // ===========================================
    // Step 6: Unsubscribe (optional)
    // ===========================================
    println!("\nStep 6: Unsubscribing from 'chat' topic...");
    let unsubscribe = json!({"Unsubscribe": {"topic": "chat"}});
    ws_stream
        .send(WsMessage::Text(unsubscribe.to_string().into()))
        .await
        .unwrap();

    println!("\nDone!");
}
