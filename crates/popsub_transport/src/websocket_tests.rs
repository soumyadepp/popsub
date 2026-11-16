//! End-to-end tests that start a real server on a free port and talk to it
//! over a WebSocket.
//!
//! Each test gets its own server, broker and temporary persistence directory,
//! configured from `Settings::default()`.

use crate::message::{ClientMessage, ServerMessage};
use crate::websocket::{RateLimiters, Server};
use futures_util::{SinkExt, StreamExt};
use popsub_broker::{Broker, SharedBroker};
use popsub_config::Settings;
use popsub_persistence::Persistence;
use std::time::Duration;
use tempfile::tempdir;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Starts a server on a free local port and returns its address.
///
/// The server listens on port 0, so the operating system picks a free port
/// and the server is accepting connections before this returns. Its broker
/// stores messages in a new temporary directory.
///
/// Args:
///     settings: The settings to configure the server's auth service from.
///     limiters: The rate limiters the server uses.
///
/// Returns:
///     The address to connect to, the temporary directory (kept alive so the
///     store isn't deleted) and the server's broker.
async fn start_server(
    settings: &Settings,
    limiters: RateLimiters,
) -> (std::net::SocketAddr, tempfile::TempDir, SharedBroker) {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let persistence = Persistence::new(temp_dir.path().to_str().unwrap(), None, None)
        .expect("Failed to create persistence");
    let broker = SharedBroker::new(Broker::new_with_persistence(
        persistence,
        settings.broker.clone(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind");
    let addr = listener.local_addr().expect("No local address");
    let server = Server::from_settings(broker.clone(), &settings.server)
        .await
        .with_rate_limiters(limiters);
    tokio::spawn(server.serve(listener));
    (addr, temp_dir, broker)
}

/// Starts a server with default settings and limits and opens a TCP connection to it.
///
/// Returns:
///     The connected stream (before the WebSocket handshake), the settings the
///     server uses, the temporary directory (kept alive so the store isn't
///     deleted) and the server's broker.
async fn setup_server_and_client() -> (
    tokio::net::TcpStream,
    Settings,
    tempfile::TempDir,
    SharedBroker,
) {
    let settings = Settings::default();
    let (addr, temp_dir, broker) = start_server(&settings, RateLimiters::default()).await;
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("Failed to connect");
    (stream, settings, temp_dir, broker)
}

/// Waits up to two seconds for the broker to have no registered clients.
///
/// Cleanup runs in the connection's own task after the socket closes, so a
/// test can't check for it straight after closing.
///
/// Args:
///     broker: The broker to watch.
///
/// Returns:
///     Whether the broker emptied in time.
async fn wait_for_no_clients(broker: &SharedBroker) -> bool {
    for _ in 0..200 {
        if broker.with(|broker| broker.clients.is_empty()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Checks that logging in with the default admin credentials returns a token
/// that `auth` accepts.
#[tokio::test]
async fn test_login_success() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

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

    let auth_msg = ClientMessage::Auth { token };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&auth_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send auth message");

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

/// Checks that a login with unknown credentials gets an `invalid credentials`
/// error.
#[tokio::test]
async fn test_login_failure() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

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

/// Checks that `auth` with a malformed token gets an `authentication failed`
/// error.
#[tokio::test]
async fn test_auth_failure_invalid_token() {
    let (stream, _settings, _temp_dir, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    let auth_msg = ClientMessage::Auth {
        token: "invalid.token.here".to_string(),
    };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&auth_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send auth message");

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

/// Checks that subscribing before `auth` gets a `must authenticate first`
/// error and the client is removed from the broker once the socket closes.
#[tokio::test]
async fn test_action_before_auth_fails() {
    let (stream, _settings, _temp_dir, broker) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    let subscribe_msg = ClientMessage::Subscribe {
        topic: "test".to_string(),
    };
    ws_stream
        .send(WsMessage::Text(
            serde_json::to_string(&subscribe_msg).unwrap().into(),
        ))
        .await
        .expect("Failed to send subscribe message");

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

    ws_stream
        .close(None)
        .await
        .expect("Failed to close WebSocket");

    let res = ws_stream
        .send(WsMessage::Text("should not send".to_string().into()))
        .await;
    assert!(res.is_err());

    assert!(wait_for_no_clients(&broker).await);
}

/// Sends one client frame and waits for the next server frame.
///
/// Panics if the send fails, the connection closes first, or the reply isn't a
/// `ServerMessage`.
///
/// Args:
///     ws_stream: The client's WebSocket connection.
///     msg: The frame to send.
///
/// Returns:
///     The server's reply.
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

/// Checks that `register` is refused with `user registration is disabled`
/// under the default settings.
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

/// Checks that five failed logins lock out that username from this address
/// while the admin can still log in from the same connection.
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

/// Checks that a publish refused for lack of permission doesn't use up the user's rate limit.
#[tokio::test]
async fn denied_publishes_do_not_spend_the_rate_limit() {
    let mut settings = Settings::default();
    settings.server.allow_registration = true;
    let limiters = RateLimiters {
        message_limiter: std::sync::Arc::new(popsub_ratelimit::TokenBucket::new(
            1,
            1,
            Duration::from_secs(3600),
        )),
        ..RateLimiters::default()
    };
    let (addr, _temp_dir, _broker) = start_server(&settings, limiters).await;
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    let credentials = ("no_topics_user".to_string(), "password123".to_string());
    send_and_receive(
        &mut ws_stream,
        &ClientMessage::Register {
            username: credentials.0.clone(),
            password: credentials.1.clone(),
        },
    )
    .await;
    let ServerMessage::LoginResponse { token } = send_and_receive(
        &mut ws_stream,
        &ClientMessage::Login {
            username: credentials.0,
            password: credentials.1,
        },
    )
    .await
    else {
        panic!("login failed");
    };
    send_and_receive(&mut ws_stream, &ClientMessage::Auth { token }).await;

    for _ in 0..3 {
        let reply = send_and_receive(
            &mut ws_stream,
            &ClientMessage::Publish {
                topic: "private".to_string(),
                payload: "hello".to_string(),
                message_id: None,
                qos: None,
            },
        )
        .await;
        assert!(
            matches!(&reply, ServerMessage::Error { message } if message.starts_with("not authorized")),
            "expected a permission error, got {reply:?}"
        );
    }
}
