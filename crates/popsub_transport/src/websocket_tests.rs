use crate::message::{ClientMessage, ServerMessage};
use crate::websocket::start_websocket_server;
use futures_util::{SinkExt, StreamExt};
use popsub_broker::Broker;
use popsub_config::Settings;
use popsub_persistence::Persistence;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio_tungstenite::tungstenite::Message as WsMessage;

async fn setup_server_and_client() -> (
    tokio::net::TcpStream,
    Settings,
    tempfile::TempDir,
    Arc<Mutex<Broker>>,
) {
    let settings = Settings::default();
    let addr = format!(
        "127.0.0.1:{}",
        portpicker::pick_unused_port().expect("No free ports")
    );

    let temp_dir = tempdir().expect("Failed to create temp dir");
    let persistence = Persistence::new(temp_dir.path().to_str().unwrap(), None, None);
    let broker = Arc::new(Mutex::new(Broker::new_with_persistence(
        persistence.expect("Failed to create persistence"),
        settings.broker.clone(),
    )));

    tokio::spawn(start_websocket_server(
        addr.clone(),
        broker.clone(),
        settings.clone(),
    ));

    // Give the server a moment to start up
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .expect("Failed to connect");
    (stream, settings, temp_dir, broker)
}

#[tokio::test]
async fn test_login_success() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    // 1. Send Login message
    let login_msg = ClientMessage::Login {
        username: "admin".to_string(),
        password: "password".to_string(),
    };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&login_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send login message");

    // 2. Receive LoginResponse
    let response = ws_stream
        .next()
        .await
        .expect("Did not receive response")
        .unwrap();
    let raw_data = response.into_data();
    let server_msg: ServerMessage = serde_json::from_slice(&raw_data).unwrap_or_else(|e| {
        panic!("Failed to deserialize ServerMessage from {raw_data:?}: {e}");
    });

    let token = match server_msg {
        ServerMessage::LoginResponse { token } => token,
        _ => panic!("Expected LoginResponse, got {server_msg:?}"),
    };

    // 3. Send Auth message with the received token
    let auth_msg = ClientMessage::Auth { token };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&auth_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send auth message");

    // 4. Receive Auth success message
    let auth_response = ws_stream
        .next()
        .await
        .expect("Did not receive auth response")
        .unwrap();
    let raw_data = auth_response.into_data();
    let server_msg: ServerMessage = serde_json::from_slice(&raw_data).unwrap_or_else(|e| {
        panic!("Failed to deserialize ServerMessage from {raw_data:?}: {e}");
    });

    match server_msg {
        ServerMessage::Authenticated {} => {}
        _ => panic!("Expected Authenticated, got {server_msg:?}"),
    };
}

#[tokio::test]
async fn test_login_failure() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    // 1. Send Login message with incorrect credentials
    let login_msg = ClientMessage::Login {
        username: "wrong_user".to_string(),
        password: "wrong_password".to_string(),
    };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&login_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send login message");

    // 2. Receive Login failure message
    let response = ws_stream
        .next()
        .await
        .expect("Did not receive response")
        .unwrap();
    let raw_data = response.into_data();
    let server_msg: ServerMessage = serde_json::from_slice(&raw_data).unwrap_or_else(|e| {
        panic!("Failed to deserialize ServerMessage from {raw_data:?}: {e}");
    });

    match server_msg {
        ServerMessage::Error { message } => {
            assert_eq!(message, "invalid credentials");
        }
        _ => panic!("Expected Error, got {server_msg:?}"),
    };
}

#[tokio::test]
async fn test_auth_failure_invalid_token() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    // 1. Send Auth message with an invalid token
    let auth_msg = ClientMessage::Auth {
        token: "invalid.token.here".to_string(),
    };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&auth_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send auth message");

    // 2. Receive Auth failure message
    let response = ws_stream
        .next()
        .await
        .expect("Did not receive response")
        .unwrap();
    let raw_data = response.into_data();
    let server_msg: ServerMessage = serde_json::from_slice(&raw_data).unwrap_or_else(|e| {
        panic!("Failed to deserialize ServerMessage from {raw_data:?}: {e}");
    });

    match server_msg {
        ServerMessage::Error { message } => {
            assert_eq!(message, "authentication failed");
        }
        _ => panic!("Expected Error, got {server_msg:?}"),
    };
}

#[tokio::test]
async fn test_action_before_auth_fails() {
    let (stream, _settings, _temp_dir, broker) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    // 1. Send a Subscribe message before authentication
    let subscribe_msg = ClientMessage::Subscribe {
        topic: "test".to_string(),
    };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&subscribe_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send subscribe message");

    // 2. Receive error message and connection close
    let response = ws_stream
        .next()
        .await
        .expect("Did not receive response")
        .unwrap();
    let raw_data = response.into_data();
    let server_msg: ServerMessage = serde_json::from_slice(&raw_data).unwrap_or_else(|e| {
        panic!("Failed to deserialize ServerMessage from {raw_data:?}: {e}");
    });

    match server_msg {
        ServerMessage::Error { message } => {
            assert_eq!(message, "must authenticate first");
        }
        _ => panic!("Expected Error, got {server_msg:?}"),
    };

    // Explicitly close the WebSocket stream
    ws_stream
        .close(None)
        .await
        .expect("Failed to close WebSocket");

    // Attempt to send another message and assert that it fails
    let res = ws_stream
        .send(WsMessage::Text("should not send".to_string().into()))
        .await;
    assert!(res.is_err());

    // Assert that the client has been cleaned up from the broker
    let broker_lock = broker.lock().unwrap();
    assert!(broker_lock.clients.is_empty());
}

async fn send_and_receive(
    ws_stream: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    msg: &ClientMessage,
) -> ServerMessage {
    ws_stream
        .send(WsMessage::Text(serde_json::to_string(msg).unwrap().into()))
        .await
        .expect("Failed to send message");
    let raw = ws_stream
        .next()
        .await
        .expect("Did not receive response")
        .unwrap()
        .into_data();
    serde_json::from_slice(&raw)
        .unwrap_or_else(|e| panic!("Failed to deserialize ServerMessage from {raw:?}: {e}"))
}

#[tokio::test]
async fn registration_is_disabled_by_default() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    let response = send_and_receive(
        &mut ws_stream,
        &ClientMessage::Register {
            username: "mallory".to_string(),
            password: "password123".to_string(),
        },
    )
    .await;

    match response {
        ServerMessage::RegisterResponse { success, message } => {
            assert!(!success);
            assert_eq!(message, "user registration is disabled");
        }
        other => panic!("Expected RegisterResponse, got {other:?}"),
    }
}

#[tokio::test]
async fn locking_out_one_username_leaves_others_usable() {
    let (stream, settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    for _ in 0..5 {
        send_and_receive(
            &mut ws_stream,
            &ClientMessage::Login {
                username: "someone_else".to_string(),
                password: "wrong_password".to_string(),
            },
        )
        .await;
    }
    let locked = send_and_receive(
        &mut ws_stream,
        &ClientMessage::Login {
            username: "someone_else".to_string(),
            password: "wrong_password".to_string(),
        },
    )
    .await;
    assert!(
        matches!(&locked, ServerMessage::Error { message } if message.starts_with("Too many login attempts")),
        "expected a lockout, got {locked:?}"
    );

    let admin = send_and_receive(
        &mut ws_stream,
        &ClientMessage::Login {
            username: settings.server.username.clone(),
            password: settings.server.password.clone(),
        },
    )
    .await;
    assert!(
        matches!(admin, ServerMessage::LoginResponse { .. }),
        "admin should still log in, got {admin:?}"
    );
}
