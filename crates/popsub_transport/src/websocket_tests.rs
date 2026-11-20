//! End-to-end tests that start a real server on a free port and talk to it
//! over a WebSocket.
//!
//! Each test gets its own server, broker and in-memory storage, configured
//! from `Settings::default()` unless it says otherwise.

use crate::websocket::{Heartbeat, RateLimiters, Server};
use futures_util::{SinkExt, StreamExt};
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::Settings;
use popsub_protocol::{ClientMessage, CloseCode, Encoding, ErrorCode, ServerMessage, StartFrom};
use popsub_storage::{Durability, FileStorage, MemoryStorage, Storage};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Starts a server on a free local port and returns its address.
///
/// The server listens on port 0, so the operating system picks a free port
/// and the server is accepting connections before this returns. Its topic
/// logs live in a new `MemoryStorage`.
///
/// Args:
///     settings: The settings to configure the server's auth service and
///         retention from.
///     limiters: The rate limiters the server uses.
///
/// Returns:
///     The address to connect to and the server's broker.
async fn start_server(
    settings: &Settings,
    limiters: RateLimiters,
) -> (std::net::SocketAddr, SharedBroker) {
    let (addr, broker, _server) = start_server_on(
        settings,
        limiters,
        Arc::new(MemoryStorage::new()),
        Durability::None,
    )
    .await;
    (addr, broker)
}

/// Starts a server on a free local port with the given storage.
///
/// Args:
///     settings: The settings to configure the server's auth service and
///         retention from.
///     limiters: The rate limiters the server uses.
///     storage: Where topic logs and sessions live.
///     durability: The durability appends are stored at.
///
/// Returns:
///     The address to connect to, the server's broker, and the task
///     accepting connections, which the caller can abort to stop the server.
async fn start_server_on(
    settings: &Settings,
    limiters: RateLimiters,
    storage: Arc<dyn Storage>,
    durability: Durability,
) -> (
    std::net::SocketAddr,
    SharedBroker,
    tokio::task::JoinHandle<()>,
) {
    let broker = SharedBroker::new(
        Broker::new(settings.broker.clone()),
        storage,
        LogSettings {
            durability,
            retention: settings.storage.retention.to_retention(),
            session_expiry: Duration::from_secs(settings.storage.session_expiry_secs),
        },
    )
    .expect("the backend accepts the requested durability");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind");
    let addr = listener.local_addr().expect("No local address");
    let server = Server::from_settings(broker.clone(), &settings.server)
        .await
        .with_rate_limiters(limiters);
    let accepting = tokio::spawn(server.serve(listener));
    (addr, broker, accepting)
}

/// Starts a server with default settings and limits and opens a TCP connection to it.
///
/// Returns:
///     The connected stream (before the WebSocket handshake), the settings the
///     server uses and the server's broker.
async fn setup_server_and_client() -> (tokio::net::TcpStream, Settings, SharedBroker) {
    let settings = Settings::default();
    let (addr, broker) = start_server(&settings, RateLimiters::default()).await;
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("Failed to connect");
    (stream, settings, broker)
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
    let (stream, _settings, _) = setup_server_and_client().await;
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
        ServerMessage::LoggedIn { token } => token,
        _ => panic!("Expected LoggedIn, got {server_msg:?}"),
    };

    let auth_msg = ClientMessage::Auth {
        token,
        protocol: None,
        session_id: None,
        clean_start: false,
        hold_resumed: false,
    };
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
        ServerMessage::Authenticated { .. } => {}
        _ => panic!("Expected Authenticated, got {server_msg:?}"),
    };
}

/// Checks that a login with unknown credentials gets an `invalid credentials`
/// error.
#[tokio::test]
async fn test_login_failure() {
    let (stream, _settings, _) = setup_server_and_client().await;
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
        ServerMessage::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::InvalidCredentials);
            assert_eq!(message, "invalid credentials");
        }
        _ => panic!("Expected Error, got {server_msg:?}"),
    };
}

/// Checks that `auth` with a malformed token gets an `authentication failed`
/// error.
#[tokio::test]
async fn test_auth_failure_invalid_token() {
    let (stream, _settings, _) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    let auth_msg = ClientMessage::Auth {
        token: "invalid.token.here".to_string(),
        protocol: None,
        session_id: None,
        clean_start: false,
        hold_resumed: false,
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
        ServerMessage::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::AuthFailed);
            assert_eq!(message, "authentication failed");
        }
        _ => panic!("Expected Error, got {server_msg:?}"),
    };
}

/// Checks that subscribing before `auth` gets a `must authenticate first`
/// error and the client is removed from the broker once the socket closes.
#[tokio::test]
async fn test_action_before_auth_fails() {
    let (stream, _settings, broker) = setup_server_and_client().await;
    let (mut ws_stream, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");

    let subscribe_msg = ClientMessage::Subscribe {
        topic: "test".to_string(),
        from: StartFrom::New,
        group: None,
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
        ServerMessage::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::Unauthenticated);
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
    let raw = next_text(ws_stream).await;
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("Failed to deserialize ServerMessage from {raw:?}: {e}"))
}

/// Reads the next text frame, skipping pings and pongs, failing the test
/// after two seconds or if the connection closes first.
///
/// Args:
///     ws: The connection.
///
/// Returns:
///     The frame's text.
async fn next_text(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> String {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("no frame within two seconds")
            .expect("connection closed")
            .expect("read failed");
        match frame {
            WsMessage::Text(text) => return text.to_string(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Reads until the server closes the connection, skipping anything else.
///
/// Args:
///     ws: The connection.
///     within: How long to wait for the close.
///
/// Returns:
///     The close code and reason.
async fn next_close(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    within: Duration,
) -> (u16, String) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("the server didn't close the connection in time");
        match frame {
            Some(Ok(WsMessage::Close(Some(close)))) => {
                return (u16::from(close.code), close.reason.to_string());
            }
            Some(Ok(_)) => continue,
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

/// Checks that `register` is refused with `user registration is disabled`
/// under the default settings.
#[tokio::test]
async fn registration_is_disabled_by_default() {
    let (stream, _settings, _) = setup_server_and_client().await;
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
        ServerMessage::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::RegistrationFailed);
            assert_eq!(message, "user registration is disabled");
        }
        other => panic!("Expected a registration error, got {other:?}"),
    }
}

/// Checks that five failed logins lock out that username from this address
/// while the admin can still log in from the same connection.
#[tokio::test]
async fn locking_out_one_username_leaves_others_usable() {
    let (stream, settings, _) = setup_server_and_client().await;
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
        matches!(
            &locked,
            ServerMessage::Error {
                code: ErrorCode::LockedOut,
                retry_after: Some(_),
                ..
            }
        ),
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
        matches!(admin, ServerMessage::LoggedIn { .. }),
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
    let (addr, _broker) = start_server(&settings, limiters).await;
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
    let ServerMessage::LoggedIn { token } = send_and_receive(
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
    send_and_receive(
        &mut ws_stream,
        &ClientMessage::Auth {
            token,
            protocol: None,
            session_id: None,
            clean_start: false,
            hold_resumed: false,
        },
    )
    .await;

    for _ in 0..3 {
        let reply = send_and_receive(
            &mut ws_stream,
            &ClientMessage::Publish {
                topic: "private".to_string(),
                payload: "hello".to_string(),
                encoding: None,
                message_id: None,
                qos: None,
            },
        )
        .await;
        assert!(
            matches!(
                &reply,
                ServerMessage::Error {
                    code: ErrorCode::NotAuthorized,
                    ..
                }
            ),
            "expected a permission error, got {reply:?}"
        );
    }
}

/// A client's WebSocket connection in these tests.
type TestSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// Connects to a server and logs in and authenticates as the admin, without a session.
///
/// Args:
///     addr: The server's address.
///     settings: The settings the server uses, for the admin credentials.
///
/// Returns:
///     An authenticated connection.
async fn admin_connection(addr: std::net::SocketAddr, settings: &Settings) -> TestSocket {
    admin_session(addr, settings, None).await.0
}

/// Connects to a server and authenticates as the admin, optionally holding
/// a durable session.
///
/// Args:
///     addr: The server's address.
///     settings: The settings the server uses, for the admin credentials.
///     session_id: The session to hold, if any.
///
/// Returns:
///     An authenticated connection and the `authenticated` reply.
async fn admin_session(
    addr: std::net::SocketAddr,
    settings: &Settings,
    session_id: Option<&str>,
) -> (TestSocket, ServerMessage) {
    admin_session_with(addr, settings, session_id, false).await
}

/// Connects to a server and authenticates as the admin, optionally holding
/// a durable session and asking the server to hold its stored subscriptions.
///
/// Args:
///     addr: The server's address.
///     settings: The settings the server uses, for the admin credentials.
///     session_id: The session to hold, if any.
///     hold_resumed: Whether to send `hold_resumed`.
///
/// Returns:
///     An authenticated connection and the `authenticated` reply.
async fn admin_session_with(
    addr: std::net::SocketAddr,
    settings: &Settings,
    session_id: Option<&str>,
    hold_resumed: bool,
) -> (TestSocket, ServerMessage) {
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("WebSocket handshake failed");
    let ServerMessage::LoggedIn { token } = send_and_receive(
        &mut ws,
        &ClientMessage::Login {
            username: settings.server.username.clone(),
            password: settings.server.password.clone(),
        },
    )
    .await
    else {
        panic!("admin login failed");
    };
    let reply = send_and_receive(
        &mut ws,
        &ClientMessage::Auth {
            token,
            protocol: None,
            session_id: session_id.map(str::to_string),
            clean_start: false,
            hold_resumed,
        },
    )
    .await;
    assert!(
        matches!(reply, ServerMessage::Authenticated { .. }),
        "{reply:?}"
    );
    (ws, reply)
}

/// Sends a frame without waiting for a reply.
///
/// Args:
///     ws: The connection.
///     msg: The frame, as JSON so tests can use the wire format directly.
async fn send_json(ws: &mut TestSocket, msg: serde_json::Value) {
    ws.send(WsMessage::Text(msg.to_string().into()))
        .await
        .expect("send");
}

/// Reads the next server frame, failing the test after two seconds.
///
/// Args:
///     ws: The connection.
///
/// Returns:
///     The frame.
async fn next_frame(ws: &mut TestSocket) -> ServerMessage {
    serde_json::from_str(&next_text(ws).await).expect("server frames parse")
}

/// Reads the next frame and expects a delivered record.
///
/// Args:
///     ws: The connection.
///
/// Returns:
///     The record's seq and payload.
async fn next_delivery(ws: &mut TestSocket) -> (u64, String) {
    match next_frame(ws).await {
        ServerMessage::Message(delivery) => (delivery.seq, delivery.payload),
        other => panic!("expected a message frame, got {other:?}"),
    }
}

/// Waits until the server has handled every frame this connection sent so far.
///
/// The server handles one connection's frames in order but doesn't confirm
/// a QoS 0 publish, so the connection subscribes to a topic of its own,
/// publishes to it, and waits for that record to come back.
///
/// Args:
///     ws: The connection.
///     name: A topic no other connection uses.
async fn settle(ws: &mut TestSocket, name: &str) {
    send_json(ws, serde_json::json!({"type": "subscribe", "topic": name})).await;
    send_json(
        ws,
        serde_json::json!({"type": "publish", "topic": name, "payload": "settled"}),
    )
    .await;
    loop {
        if let ServerMessage::Message(delivery) = next_frame(ws).await
            && delivery.topic == name
        {
            return;
        }
    }
}

/// Checks that a publish reaches a subscriber as a tagged `message` frame
/// with a seq, and that retrying the same message id doesn't deliver it twice.
#[tokio::test]
async fn published_messages_arrive_tagged_with_seq() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut subscriber = admin_connection(addr, &settings).await;
    let mut publisher = admin_connection(addr, &settings).await;

    send_json(
        &mut subscriber,
        serde_json::json!({"type": "subscribe", "topic": "chat"}),
    )
    .await;
    settle(&mut subscriber, "settle-subscriber").await;

    for (payload, id) in [("one", "id-1"), ("one", "id-1"), ("two", "id-2")] {
        send_json(
            &mut publisher,
            serde_json::json!({"type": "publish", "topic": "chat", "payload": payload, "message_id": id}),
        )
        .await;
    }
    assert_eq!(next_delivery(&mut subscriber).await, (1, "one".to_string()));
    assert_eq!(next_delivery(&mut subscriber).await, (2, "two".to_string()));
}

/// Checks that a subscriber asking for `last` gets those records with their
/// original seq, and one asking for trimmed history is told first.
#[tokio::test]
async fn subscribers_replay_the_history_they_ask_for() {
    let mut settings = Settings::default();
    settings.storage.retention.max_messages = 3;
    let (addr, broker) = start_server(&settings, RateLimiters::default()).await;
    let mut publisher = admin_connection(addr, &settings).await;
    for n in 1..=5 {
        send_json(
            &mut publisher,
            serde_json::json!({"type": "publish", "topic": "chat", "payload": format!("m{n}")}),
        )
        .await;
    }
    settle(&mut publisher, "settle-publisher").await;

    let mut recent = admin_connection(addr, &settings).await;
    send_json(
        &mut recent,
        serde_json::json!({"type": "subscribe", "topic": "chat", "from": {"last": 2}}),
    )
    .await;
    match next_frame(&mut recent).await {
        ServerMessage::Subscribed(s) => {
            assert_eq!(
                (s.topic.as_str(), s.replay_from, s.replay_to),
                ("chat", 4, 5)
            )
        }
        other => panic!("expected subscribed, got {other:?}"),
    }
    assert_eq!(next_delivery(&mut recent).await, (4, "m4".to_string()));
    assert_eq!(next_delivery(&mut recent).await, (5, "m5".to_string()));

    let mut everything = admin_connection(addr, &settings).await;
    send_json(
        &mut everything,
        serde_json::json!({"type": "subscribe", "topic": "chat", "from": {"seq": 1}}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut everything).await,
        ServerMessage::Subscribed(_)
    ));
    match next_frame(&mut everything).await {
        ServerMessage::Truncated(t) => {
            assert_eq!((t.topic.as_str(), t.first_available), ("chat", 3))
        }
        other => panic!("expected truncated, got {other:?}"),
    }
    assert_eq!(next_delivery(&mut everything).await.0, 3);

    let subscribed = broker
        .subscribe("chat", "not-a-client", StartFrom::New, None)
        .await
        .expect("subscribe");
    assert!(!subscribed.added);
}

/// Checks that a QoS 1 publish is confirmed with its id and seq, including a
/// retry of the same id.
#[tokio::test]
async fn qos1_publishes_are_confirmed() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut publisher = admin_connection(addr, &settings).await;
    for _ in 0..2 {
        send_json(
            &mut publisher,
            serde_json::json!({"type": "publish", "topic": "chat", "payload": "x", "message_id": "m-1", "qos": 1}),
        )
        .await;
        match next_frame(&mut publisher).await {
            ServerMessage::Published { message_id, seq } => {
                assert_eq!((message_id.as_str(), seq), ("m-1", 1))
            }
            other => panic!("expected published, got {other:?}"),
        }
    }
}

/// Checks that a durable session gets back, after reconnecting, the QoS 1
/// record it never acknowledged and the one published while it was away,
/// while an acknowledged record isn't sent again.
#[tokio::test]
async fn a_session_resumes_after_reconnecting() {
    let settings = Settings::default();
    let (addr, broker) = start_server(&settings, RateLimiters::default()).await;
    let mut publisher = admin_connection(addr, &settings).await;
    let publish = |payload: &str| serde_json::json!({"type": "publish", "topic": "chat", "payload": payload, "qos": 1});

    let (mut phone, reply) = admin_session(addr, &settings, Some("phone")).await;
    assert!(
        matches!(&reply, ServerMessage::Authenticated { session_id: Some(id), resumed, .. } if id == "phone" && resumed.is_empty()),
        "{reply:?}"
    );
    send_json(
        &mut phone,
        serde_json::json!({"type": "subscribe", "topic": "chat"}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut phone).await,
        ServerMessage::Subscribed(_)
    ));
    for payload in ["one", "two"] {
        send_json(&mut publisher, publish(payload)).await;
        next_frame(&mut publisher).await;
    }
    assert_eq!(next_delivery(&mut phone).await, (1, "one".to_string()));
    assert_eq!(next_delivery(&mut phone).await, (2, "two".to_string()));
    send_json(
        &mut phone,
        serde_json::json!({"type": "ack", "topic": "chat", "seq": 1}),
    )
    .await;
    settle(&mut phone, "settle-phone").await;
    phone.close(None).await.expect("close");
    drop(phone);
    assert!(wait_for_no_session(&broker).await);

    send_json(&mut publisher, publish("three")).await;
    next_frame(&mut publisher).await;

    let (mut phone, reply) = admin_session(addr, &settings, Some("phone")).await;
    assert!(
        matches!(&reply, ServerMessage::Authenticated { resumed, .. } if resumed.iter().any(|t| t == "chat")),
        "{reply:?}"
    );
    let mut received = Vec::new();
    while received.len() < 2 {
        match next_frame(&mut phone).await {
            ServerMessage::Message(d) if d.topic == "chat" => received.push((d.seq, d.payload)),
            ServerMessage::Subscribed(s) if s.topic == "chat" => assert_eq!(s.replay_from, 2),
            _ => {}
        }
    }
    assert_eq!(
        received,
        vec![(2, "two".to_string()), (3, "three".to_string())]
    );
}

/// Checks `hold_resumed`: the session's topic is listed but nothing is sent
/// or put in flight until the client subscribes, which then replays from the
/// stored cursor; and unsubscribing a topic that was never attached forgets it.
#[tokio::test]
async fn a_held_session_sends_nothing_until_the_client_subscribes() {
    let settings = Settings::default();
    let (addr, broker) = start_server(&settings, RateLimiters::default()).await;
    let mut publisher = admin_connection(addr, &settings).await;
    let publish = |payload: &str| serde_json::json!({"type": "publish", "topic": "chat", "payload": payload, "qos": 1});

    let (mut phone, _) = admin_session(addr, &settings, Some("phone")).await;
    send_json(
        &mut phone,
        serde_json::json!({"type": "subscribe", "topic": "chat"}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut phone).await,
        ServerMessage::Subscribed(_)
    ));
    for payload in ["one", "two"] {
        send_json(&mut publisher, publish(payload)).await;
        next_frame(&mut publisher).await;
    }
    assert_eq!(next_delivery(&mut phone).await, (1, "one".to_string()));
    assert_eq!(next_delivery(&mut phone).await, (2, "two".to_string()));
    phone.close(None).await.expect("close");
    drop(phone);
    assert!(wait_for_no_session(&broker).await);

    let (mut phone, reply) = admin_session_with(addr, &settings, Some("phone"), true).await;
    assert!(
        matches!(&reply, ServerMessage::Authenticated { resumed, held: true, .. } if resumed == &["chat".to_string()]),
        "{reply:?}"
    );
    send_json(&mut phone, serde_json::json!({"type": "ping", "id": 1})).await;
    assert!(
        matches!(next_frame(&mut phone).await, ServerMessage::Pong {}),
        "nothing should arrive before the client subscribes"
    );
    assert_eq!(broker.stats().in_flight, 0);

    send_json(
        &mut phone,
        serde_json::json!({"type": "subscribe", "topic": "chat"}),
    )
    .await;
    match next_frame(&mut phone).await {
        ServerMessage::Subscribed(s) => assert_eq!(s.replay_from, 1),
        other => panic!("expected subscribed, got {other:?}"),
    }
    assert_eq!(next_delivery(&mut phone).await, (1, "one".to_string()));
    assert_eq!(next_delivery(&mut phone).await, (2, "two".to_string()));
    phone.close(None).await.expect("close");
    drop(phone);
    assert!(wait_for_no_session(&broker).await);

    let (mut phone, _) = admin_session_with(addr, &settings, Some("phone"), true).await;
    send_json(
        &mut phone,
        serde_json::json!({"type": "unsubscribe", "topic": "chat", "id": 2}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut phone).await,
        ServerMessage::Unsubscribed { .. }
    ));
    phone.close(None).await.expect("close");
    drop(phone);
    assert!(wait_for_no_session(&broker).await);
    let (_phone, reply) = admin_session_with(addr, &settings, Some("phone"), true).await;
    assert!(
        matches!(&reply, ServerMessage::Authenticated { resumed, .. } if resumed.is_empty()),
        "{reply:?}"
    );
}

/// Checks that a publish with `encoding: "base64"` is delivered with the
/// same payload and encoding, live and on replay, and that a payload that
/// isn't valid base64 is refused with `bad_request` and stores nothing.
#[tokio::test]
async fn base64_payloads_are_carried_and_checked() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut ws = admin_connection(addr, &settings).await;
    send_json(
        &mut ws,
        serde_json::json!({"type": "subscribe", "topic": "blobs"}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut ws).await,
        ServerMessage::Subscribed(_)
    ));

    send_json(
        &mut ws,
        serde_json::json!({"type": "publish", "id": 1, "topic": "blobs", "payload": "not base64!", "encoding": "base64", "qos": 1}),
    )
    .await;
    assert!(
        matches!(
            next_frame(&mut ws).await,
            ServerMessage::Error {
                code: ErrorCode::BadRequest,
                ..
            }
        ),
        "an invalid base64 payload should be refused"
    );

    send_json(
        &mut ws,
        serde_json::json!({"type": "publish", "id": 2, "topic": "blobs", "payload": "AP/+gA==", "encoding": "base64", "qos": 1}),
    )
    .await;
    let mut delivered = None;
    let mut published = None;
    while delivered.is_none() || published.is_none() {
        match next_frame(&mut ws).await {
            ServerMessage::Message(d) => delivered = Some(d),
            ServerMessage::Published { seq, .. } => published = Some(seq),
            other => panic!("unexpected frame {other:?}"),
        }
    }
    let delivered = delivered.expect("a delivery");
    assert_eq!(published, Some(1), "the refused publish stored nothing");
    assert_eq!(
        (
            delivered.seq,
            delivered.payload.as_str(),
            delivered.encoding
        ),
        (1, "AP/+gA==", Some(Encoding::Base64))
    );

    let mut late = admin_connection(addr, &settings).await;
    send_json(
        &mut late,
        serde_json::json!({"type": "subscribe", "topic": "blobs", "from": {"seq": 1}}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut late).await,
        ServerMessage::Subscribed(_)
    ));
    match next_frame(&mut late).await {
        ServerMessage::Message(d) => assert_eq!(
            (d.payload.as_str(), d.encoding),
            ("AP/+gA==", Some(Encoding::Base64))
        ),
        other => panic!("expected the replayed record, got {other:?}"),
    }
}

/// Waits up to two seconds for no connection to hold a durable session.
///
/// Args:
///     broker: The broker to watch.
///
/// Returns:
///     Whether every session was released in time.
async fn wait_for_no_session(broker: &SharedBroker) -> bool {
    for _ in 0..200 {
        if broker.with(|broker| broker.connected_sessions().is_empty()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Checks that a durable session on the file backend resumes from its
/// stored cursor after the whole server is stopped and a new one is started
/// on the same data directory.
#[tokio::test]
async fn a_session_resumes_after_a_real_restart() {
    let settings = Settings::default();
    let dir = tempfile::tempdir().expect("temp dir");
    let storage = FileStorage::open(dir.path()).await.expect("open storage");
    let (addr, broker, accepting) = start_server_on(
        &settings,
        RateLimiters::default(),
        Arc::new(storage),
        Durability::Synced,
    )
    .await;
    let mut publisher = admin_connection(addr, &settings).await;
    let (mut phone, _) = admin_session(addr, &settings, Some("phone")).await;
    send_json(
        &mut phone,
        serde_json::json!({"type": "subscribe", "topic": "chat"}),
    )
    .await;
    assert!(matches!(
        next_frame(&mut phone).await,
        ServerMessage::Subscribed(_)
    ));
    for payload in ["one", "two"] {
        send_json(
            &mut publisher,
            serde_json::json!({"type": "publish", "topic": "chat", "payload": payload, "qos": 1}),
        )
        .await;
        assert!(matches!(
            next_frame(&mut publisher).await,
            ServerMessage::Published { .. }
        ));
    }
    assert_eq!(next_delivery(&mut phone).await.0, 1);
    assert_eq!(next_delivery(&mut phone).await.0, 2);
    send_json(
        &mut phone,
        serde_json::json!({"type": "ack", "topic": "chat", "seq": 1}),
    )
    .await;
    settle(&mut phone, "settle-phone").await;
    phone.close(None).await.expect("close");
    publisher.close(None).await.expect("close");
    drop((phone, publisher));
    assert!(wait_for_no_clients(&broker).await);
    accepting.abort();
    drop(broker);

    let mut reopened = None;
    for _ in 0..200 {
        match FileStorage::open(dir.path()).await {
            Ok(storage) => {
                reopened = Some(storage);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    let storage = reopened.expect("the old server released the data directory");
    let (addr, _broker, _accepting) = start_server_on(
        &settings,
        RateLimiters::default(),
        Arc::new(storage),
        Durability::Synced,
    )
    .await;
    let (mut phone, reply) = admin_session(addr, &settings, Some("phone")).await;
    assert!(
        matches!(&reply, ServerMessage::Authenticated { resumed, .. } if resumed.iter().any(|t| t == "chat")),
        "{reply:?}"
    );
    match next_frame(&mut phone).await {
        ServerMessage::Subscribed(s) => assert_eq!((s.replay_from, s.replay_to), (2, 2)),
        other => panic!("expected subscribed, got {other:?}"),
    }
    assert_eq!(next_delivery(&mut phone).await, (2, "two".to_string()));
}

/// Sends raw text and reads the reply as JSON, so tests can check ids and
/// error codes exactly as a client sees them.
///
/// Args:
///     ws: The connection.
///     text: The frame to send, which need not be valid.
///
/// Returns:
///     The reply as JSON.
async fn send_raw(ws: &mut TestSocket, text: &str) -> serde_json::Value {
    ws.send(WsMessage::Text(text.to_string().into()))
        .await
        .expect("send");
    serde_json::from_str(&next_text(ws).await).expect("replies are JSON")
}

/// Checks that replies carry the request's id, numeric or string, and that
/// `authenticated` reports the protocol version and the server's limits.
#[tokio::test]
async fn replies_echo_request_ids() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake");
    let login = send_raw(
        &mut ws,
        &serde_json::json!({"type": "login", "id": 1, "username": "admin", "password": "password"})
            .to_string(),
    )
    .await;
    assert_eq!(login["type"], "logged_in");
    assert_eq!(login["id"], 1);
    let token = login["token"].as_str().expect("token").to_string();
    let auth = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "id": "a", "token": token, "protocol": 1}).to_string(),
    )
    .await;
    assert_eq!(auth["type"], "authenticated");
    assert_eq!(auth["id"], "a");
    assert_eq!(auth["protocol"], 1);
    assert_eq!(auth["limits"]["max_inflight"], 100);
    assert_eq!(auth["limits"]["session_expiry_secs"], 86_400);
    assert_eq!(auth["limits"]["heartbeat_secs"], 30);
    assert_eq!(auth["limits"]["idle_timeout_secs"], 90);
    assert_eq!(auth["limits"]["max_payload_bytes"], 1_048_576);
    assert_eq!(auth["limits"]["max_topic_bytes"], 120);
    let subscribed = send_raw(
        &mut ws,
        &serde_json::json!({"type": "subscribe", "id": 2, "topic": "chat"}).to_string(),
    )
    .await;
    assert_eq!(subscribed["type"], "subscribed");
    assert_eq!(subscribed["id"], 2);
    let unsubscribed = send_raw(
        &mut ws,
        &serde_json::json!({"type": "unsubscribe", "id": 3, "topic": "chat"}).to_string(),
    )
    .await;
    assert_eq!(
        unsubscribed,
        serde_json::json!({"type": "unsubscribed", "id": 3, "topic": "chat"})
    );
    let published = send_raw(
        &mut ws,
        &serde_json::json!({"type": "publish", "id": 4, "topic": "chat", "payload": "x", "qos": 1})
            .to_string(),
    )
    .await;
    assert_eq!(published["type"], "published");
    assert_eq!(published["id"], 4);
}

/// Checks that a frame that doesn't parse gets `bad_request`, with its id
/// when one can be read, and the connection stays open.
#[tokio::test]
async fn bad_frames_get_bad_request_and_keep_the_connection() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake");
    let not_json = send_raw(&mut ws, "this is not json").await;
    assert_eq!(not_json["type"], "error");
    assert_eq!(not_json["code"], "bad_request");
    assert!(not_json.get("id").is_none());
    let unknown = send_raw(&mut ws, r#"{"type": "teleport", "id": 9}"#).await;
    assert_eq!(unknown["code"], "bad_request");
    assert_eq!(unknown["id"], 9);
    let missing = send_raw(
        &mut ws,
        r#"{"type": "login", "id": "x", "username": "admin"}"#,
    )
    .await;
    assert_eq!(missing["code"], "bad_request");
    assert_eq!(missing["id"], "x");
    let still_open = send_raw(
        &mut ws,
        &serde_json::json!({"type": "login", "username": "admin", "password": "password"})
            .to_string(),
    )
    .await;
    assert_eq!(still_open["type"], "logged_in");
}

/// Checks that asking for a protocol version the server doesn't speak gets
/// `unsupported_protocol` and closes the connection.
#[tokio::test]
async fn an_unsupported_protocol_version_is_refused() {
    let settings = Settings::default();
    let (addr, broker) = start_server(&settings, RateLimiters::default()).await;
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake");
    let login = send_raw(
        &mut ws,
        &serde_json::json!({"type": "login", "username": "admin", "password": "password"})
            .to_string(),
    )
    .await;
    let token = login["token"].as_str().expect("token").to_string();
    let refused = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "token": token, "protocol": 2}).to_string(),
    )
    .await;
    assert_eq!(refused["code"], "unsupported_protocol");
    assert_eq!(
        next_close(&mut ws, Duration::from_secs(2)).await,
        (4006, "unsupported_protocol".to_string())
    );
    assert!(wait_for_no_clients(&broker).await);
}

/// Opens a WebSocket to a server without logging in.
///
/// Args:
///     addr: The server's address.
///
/// Returns:
///     The connection.
async fn raw_connection(addr: std::net::SocketAddr) -> TestSocket {
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake")
        .0
}

/// Signs a token for a user that expires in a few seconds, with the server's secret.
///
/// Args:
///     settings: The server's settings, for the JWT secret.
///     username: The token's subject.
///     lifetime: How long until it expires.
///
/// Returns:
///     The token.
fn short_token(settings: &Settings, username: &str, lifetime: Duration) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &serde_json::json!({"sub": username, "iat": now, "exp": now + lifetime.as_secs()}),
        &jsonwebtoken::EncodingKey::from_secret(settings.server.jwt_secret.as_bytes()),
    )
    .expect("sign a token")
}

/// Checks that a bad token and a request before `auth` close the connection
/// with their close codes after the error.
#[tokio::test]
async fn auth_failures_close_with_their_codes() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut bad_token = raw_connection(addr).await;
    let reply = send_raw(&mut bad_token, r#"{"type": "auth", "token": "nope"}"#).await;
    assert_eq!(reply["code"], "auth_failed");
    assert_eq!(
        next_close(&mut bad_token, Duration::from_secs(2)).await,
        (CloseCode::AuthFailed.code(), "auth_failed".to_string())
    );
    let mut too_early = raw_connection(addr).await;
    let reply = send_raw(&mut too_early, r#"{"type": "subscribe", "topic": "chat"}"#).await;
    assert_eq!(reply["code"], "unauthenticated");
    assert_eq!(
        next_close(&mut too_early, Duration::from_secs(2)).await.0,
        CloseCode::Unauthenticated.code()
    );
}

/// Checks that a client that keeps answering pings stays connected, and one
/// that goes silent is closed with `idle_timeout`.
#[tokio::test]
async fn silent_connections_are_closed_after_the_idle_timeout() {
    let settings = Settings::default();
    let (addr, broker) = start_server_with(
        &settings,
        Heartbeat {
            interval: Duration::from_millis(50),
            idle_timeout: Duration::from_millis(250),
        },
    )
    .await;
    let mut attentive = raw_connection(addr).await;
    let mut pings = 0;
    let until = tokio::time::Instant::now() + Duration::from_millis(600);
    while let Ok(frame) = tokio::time::timeout_at(until, attentive.next()).await {
        match frame {
            Some(Ok(WsMessage::Ping(_))) => pings += 1,
            other => panic!("expected only pings, got {other:?}"),
        }
    }
    assert!(pings >= 3, "only {pings} pings");

    let still_there = send_raw(
        &mut attentive,
        &serde_json::json!({"type": "login", "username": "admin", "password": "password"})
            .to_string(),
    )
    .await;
    assert_eq!(still_there["type"], "logged_in");

    let mut silent = raw_connection(addr).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        next_close(&mut silent, Duration::from_secs(2)).await,
        (CloseCode::IdleTimeout.code(), "idle_timeout".to_string())
    );
    drop(broker);
}

/// Checks that a client is warned before its token expires and closed with
/// `token_expired` when it does.
#[tokio::test]
async fn expiring_tokens_are_warned_about_then_closed() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut ws = raw_connection(addr).await;
    let token = short_token(&settings, "admin", Duration::from_secs(2));
    let auth = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "token": token}).to_string(),
    )
    .await;
    assert_eq!(auth["type"], "authenticated");
    match next_frame(&mut ws).await {
        ServerMessage::AuthExpiring { expires_at_ms } => {
            assert!(expires_at_ms > wall_clock_ms());
        }
        other => panic!("expected auth_expiring, got {other:?}"),
    }
    assert_eq!(
        next_close(&mut ws, Duration::from_secs(4)).await,
        (CloseCode::TokenExpired.code(), "token_expired".to_string())
    );
}

/// The current time in milliseconds since the Unix epoch.
///
/// Returns:
///     The time.
fn wall_clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

/// Checks that sending `auth` again with a fresh token keeps the connection
/// open past the old token's expiry, and that a token for another user is
/// refused without closing the connection.
///
/// Every password check (registration and both logins) runs before the
/// short token is used, on another connection. Argon2 is slow in debug
/// builds, and on a slow CI runner those checks once outlasted the short
/// token, so only cheap token checks happen inside its lifetime.
#[tokio::test]
async fn re_auth_refreshes_the_token_for_the_same_user_only() {
    let mut settings = Settings::default();
    settings.server.allow_registration = true;
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut other = raw_connection(addr).await;
    let registered = send_raw(
        &mut other,
        r#"{"type": "register", "username": "bobby", "password": "password123"}"#,
    )
    .await;
    assert_eq!(registered["type"], "registered");
    let bob = send_raw(
        &mut other,
        r#"{"type": "login", "username": "bobby", "password": "password123"}"#,
    )
    .await;
    let bob_token = bob["token"].as_str().expect("token").to_string();
    let login = send_raw(
        &mut other,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await;
    let fresh = login["token"].as_str().expect("token").to_string();

    let mut ws = raw_connection(addr).await;
    let short = short_token(&settings, "admin", Duration::from_secs(3));
    let auth = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "token": short}).to_string(),
    )
    .await;
    assert_eq!(auth["type"], "authenticated");
    assert!(matches!(
        next_frame(&mut ws).await,
        ServerMessage::AuthExpiring { .. }
    ));

    let refused = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "id": 1, "token": bob_token}).to_string(),
    )
    .await;
    assert_eq!(
        (refused["id"].clone(), refused["code"].clone()),
        (serde_json::json!(1), serde_json::json!("auth_failed"))
    );

    let refreshed = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "id": 2, "token": fresh}).to_string(),
    )
    .await;
    assert_eq!(refreshed["type"], "authenticated");
    assert_eq!(refreshed["id"], 2);
    assert!(refreshed.get("resumed").is_none());

    tokio::time::sleep(Duration::from_secs(4)).await;
    let published = send_raw(
        &mut ws,
        r#"{"type": "publish", "topic": "chat", "payload": "x", "qos": 1}"#,
    )
    .await;
    assert_eq!(published["type"], "published");
}

/// Checks that a session taken over by a new connection closes the old one
/// with `session_taken_over`.
#[tokio::test]
async fn a_taken_over_session_closes_the_old_connection() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let (mut old, _) = admin_session(addr, &settings, Some("phone")).await;
    let (_new, _) = admin_session(addr, &settings, Some("phone")).await;
    assert_eq!(
        next_close(&mut old, Duration::from_secs(2)).await,
        (
            CloseCode::SessionTakenOver.code(),
            "session_taken_over".to_string()
        )
    );
}

/// Checks that closing every connection for a shutdown sends `going_away`.
#[tokio::test]
async fn shutting_down_sends_going_away() {
    let settings = Settings::default();
    let (addr, broker) = start_server(&settings, RateLimiters::default()).await;
    let mut ws = admin_connection(addr, &settings).await;
    assert_eq!(broker.close_all(CloseCode::GoingAway), 1);
    assert_eq!(
        next_close(&mut ws, Duration::from_secs(2)).await,
        (1001, "going_away".to_string())
    );
}

/// Starts a server with default settings, default limits and a chosen heartbeat.
///
/// Args:
///     settings: The settings to configure the server from.
///     heartbeat: The ping interval and idle timeout.
///
/// Returns:
///     The address to connect to and the server's broker.
async fn start_server_with(
    settings: &Settings,
    heartbeat: Heartbeat,
) -> (std::net::SocketAddr, SharedBroker) {
    let broker = SharedBroker::new(
        Broker::new(settings.broker.clone()),
        Arc::new(MemoryStorage::new()),
        LogSettings {
            durability: Durability::None,
            retention: settings.storage.retention.to_retention(),
            session_expiry: Duration::from_secs(settings.storage.session_expiry_secs),
        },
    )
    .expect("memory storage accepts durability none");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let server = Server::from_settings(broker.clone(), &settings.server)
        .await
        .with_heartbeat(heartbeat);
    tokio::spawn(server.serve(listener));
    (addr, broker)
}

/// Checks that a payload at the limit is published, one byte over gets
/// `payload_too_large`, and a frame far over the WebSocket limit closes the
/// connection with `message_too_big`.
#[tokio::test]
async fn oversized_payloads_and_frames_are_refused() {
    let mut settings = Settings::default();
    settings.server.max_payload_bytes = 100;
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut ws = admin_connection(addr, &settings).await;
    let at_limit = send_raw(
        &mut ws,
        &serde_json::json!({"type": "publish", "id": 1, "topic": "chat", "payload": "x".repeat(100), "qos": 1}).to_string(),
    )
    .await;
    assert_eq!(at_limit["type"], "published");
    let over = send_raw(
        &mut ws,
        &serde_json::json!({"type": "publish", "id": 2, "topic": "chat", "payload": "x".repeat(101), "qos": 1}).to_string(),
    )
    .await;
    assert_eq!(
        (over["id"].clone(), over["code"].clone()),
        (serde_json::json!(2), serde_json::json!("payload_too_large"))
    );

    let huge =
        serde_json::json!({"type": "publish", "topic": "chat", "payload": "x".repeat(70_000)})
            .to_string();
    ws.send(WsMessage::Text(huge.into())).await.expect("send");
    assert_eq!(
        next_close(&mut ws, Duration::from_secs(2)).await,
        (
            CloseCode::MessageTooBig.code(),
            "message_too_big".to_string()
        )
    );
}

/// Checks that invalid topic names get `invalid_topic` on subscribe and
/// publish, and an over-long `message_id` gets `bad_request`, without
/// closing the connection.
#[tokio::test]
async fn invalid_topics_and_message_ids_are_refused() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut ws = admin_connection(addr, &settings).await;
    let long_topic = "t".repeat(121);
    for frame in [
        serde_json::json!({"type": "subscribe", "id": 1, "topic": "metrics/#/x"}),
        serde_json::json!({"type": "publish", "id": 2, "topic": "a/+/b", "payload": "x"}),
        serde_json::json!({"type": "publish", "id": 3, "topic": "", "payload": "x"}),
        serde_json::json!({"type": "publish", "id": 4, "topic": long_topic, "payload": "x"}),
    ] {
        let reply = send_raw(&mut ws, &frame.to_string()).await;
        assert_eq!(reply["code"], "invalid_topic", "{frame}");
        assert_eq!(reply["id"], frame["id"]);
    }
    let long_id = send_raw(
        &mut ws,
        &serde_json::json!({"type": "publish", "id": 5, "topic": "chat", "payload": "x", "message_id": "m".repeat(129)}).to_string(),
    )
    .await;
    assert_eq!(long_id["code"], "bad_request");
    let fine = send_raw(
        &mut ws,
        &serde_json::json!({"type": "publish", "id": 6, "topic": "chat", "payload": "x", "qos": 1})
            .to_string(),
    )
    .await;
    assert_eq!(fine["type"], "published");
}

/// Checks that a subscriber that stops reading doesn't make the server queue
/// without limit: its queue stays near the soft limit while thousands of
/// messages are published, and when it reads again it receives every one,
/// in order, caught up from the log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_reader_is_caught_up_from_the_log_without_unbounded_queueing() {
    let mut settings = Settings::default();
    settings.server.outbound_queue_bytes = 1_000_000;
    settings.storage.retention.max_messages = 10_000;
    let limiters = RateLimiters {
        message_limiter: Arc::new(popsub_ratelimit::TokenBucket::new(
            100_000,
            100_000,
            Duration::from_secs(1),
        )),
        ..RateLimiters::default()
    };
    let (addr, broker) = start_server(&settings, limiters).await;
    let mut reader = admin_connection(addr, &settings).await;
    let subscribed = send_raw(
        &mut reader,
        &serde_json::json!({"type": "subscribe", "topic": "flood"}).to_string(),
    )
    .await;
    assert_eq!(subscribed["type"], "subscribed");

    let mut publisher = admin_connection(addr, &settings).await;
    let payload = "x".repeat(1_000);
    for n in 0..3_000 {
        send_json(
            &mut publisher,
            serde_json::json!({"type": "publish", "topic": "flood", "payload": payload, "message_id": format!("m{n}")}),
        )
        .await;
    }
    settle(&mut publisher, "settle-flood").await;

    let queued = broker.with(|b| {
        b.clients
            .values()
            .map(|c| c.sender.queued_bytes())
            .max()
            .unwrap_or(0)
    });
    assert!(queued < 70 * 1024, "a connection queued {queued} bytes");

    let mut seqs = Vec::new();
    while seqs.len() < 3_000 {
        match next_frame(&mut reader).await {
            ServerMessage::Message(delivery) => seqs.push(delivery.seq),
            other => panic!("expected only messages, got {other:?}"),
        }
    }
    assert_eq!(seqs, (1..=3_000).collect::<Vec<_>>());
}

/// Checks that an account registered on one server can log in on a new
/// server using the same users file, as after a restart.
#[tokio::test]
async fn accounts_survive_a_restart_with_a_users_file() {
    let mut settings = Settings::default();
    settings.server.allow_registration = true;
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("users.json");
    let start = |settings: Settings, path: std::path::PathBuf| async move {
        let broker = SharedBroker::new(
            Broker::new(settings.broker.clone()),
            Arc::new(MemoryStorage::new()),
            LogSettings {
                durability: Durability::None,
                retention: settings.storage.retention.to_retention(),
                session_expiry: Duration::from_secs(60),
            },
        )
        .expect("broker");
        let users = Arc::new(crate::FileUserStore::open(&path).expect("users file"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = Server::from_settings_with_users(broker, &settings.server, users).await;
        (addr, tokio::spawn(server.serve(listener)))
    };

    let (addr, first) = start(settings.clone(), path.clone()).await;
    let mut ws = raw_connection(addr).await;
    let registered = send_raw(
        &mut ws,
        r#"{"type": "register", "username": "dana", "password": "dana-password"}"#,
    )
    .await;
    assert_eq!(registered["type"], "registered");
    first.abort();
    drop(ws);

    let (addr, _second) = start(settings, path).await;
    let mut ws = raw_connection(addr).await;
    let login = send_raw(
        &mut ws,
        r#"{"type": "login", "username": "dana", "password": "dana-password"}"#,
    )
    .await;
    assert_eq!(login["type"], "logged_in");
}

/// Logs in and authenticates as a user over a new connection.
///
/// Args:
///     addr: The server's address.
///     username: The account.
///     password: Its password.
///
/// Returns:
///     The authenticated connection and the token it used.
async fn user_connection(
    addr: std::net::SocketAddr,
    username: &str,
    password: &str,
) -> (TestSocket, String) {
    let mut ws = raw_connection(addr).await;
    let login = send_raw(
        &mut ws,
        &serde_json::json!({"type": "login", "username": username, "password": password})
            .to_string(),
    )
    .await;
    assert_eq!(login["type"], "logged_in", "{login}");
    let token = login["token"].as_str().expect("a token").to_string();
    let reply = send_raw(
        &mut ws,
        &serde_json::json!({"type": "auth", "token": token}).to_string(),
    )
    .await;
    assert_eq!(reply["type"], "authenticated", "{reply}");
    (ws, token)
}

/// Sends a request as JSON and returns the reply, failing the test if it
/// doesn't echo the request's id.
///
/// Args:
///     ws: The connection.
///     request: The request, which must carry an `id`.
///
/// Returns:
///     The reply.
async fn request(ws: &mut TestSocket, request: serde_json::Value) -> serde_json::Value {
    let id = request["id"].clone();
    let reply = send_raw(ws, &request.to_string()).await;
    assert_eq!(reply["id"], id, "{reply}");
    reply
}

/// Checks that an admin can create an account that can then log in, and
/// that the account can only use topics once they are granted.
#[tokio::test]
async fn admins_create_accounts_and_grant_topics() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    let created = request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 1, "username": "frank", "password": "frank-password", "role": {"kind": "user"}}),
    )
    .await;
    assert_eq!(created["type"], "user", "{created}");
    assert_eq!(created["user"]["username"], "frank");
    assert_eq!(created["user"]["enabled"], true);
    assert_eq!(created["user"]["role"], serde_json::json!({"kind": "user"}));

    let (mut frank, _) = user_connection(addr, "frank", "frank-password").await;
    let denied = request(
        &mut frank,
        serde_json::json!({"type": "publish", "id": 1, "topic": "orders", "payload": "x", "qos": 1}),
    )
    .await;
    assert_eq!(denied["code"], "not_authorized", "{denied}");

    let granted = request(
        &mut admin,
        serde_json::json!({"type": "grant", "id": 2, "username": "frank", "pattern": "orders/#", "access": "publish"}),
    )
    .await;
    assert_eq!(
        granted["user"]["role"]["topics"],
        serde_json::json!([{"pattern": "orders/#", "access": "publish"}])
    );
    let published = request(
        &mut frank,
        serde_json::json!({"type": "publish", "id": 2, "topic": "orders", "payload": "x", "qos": 1}),
    )
    .await;
    assert_eq!(published["type"], "published", "{published}");

    let listed = request(
        &mut admin,
        serde_json::json!({"type": "list_users", "id": 3}),
    )
    .await;
    let names: Vec<&str> = listed["users"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|u| u["username"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, vec!["admin", "frank"]);
}

/// Checks that admin requests from anyone but an admin are refused with
/// `forbidden` and change nothing.
#[tokio::test]
async fn admin_requests_need_an_admin() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 1, "username": "erin", "password": "erin-password", "role": {"kind": "user", "topics": [{"pattern": "#", "access": "both"}]}}),
    )
    .await;
    let (mut erin, _) = user_connection(addr, "erin", "erin-password").await;
    for (n, attempt) in [
        serde_json::json!({"type": "list_users"}),
        serde_json::json!({"type": "set_role", "username": "erin", "role": {"kind": "admin"}}),
        serde_json::json!({"type": "delete_user", "username": "admin"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut attempt = attempt;
        attempt["id"] = serde_json::json!(n);
        let reply = request(&mut erin, attempt).await;
        assert_eq!(reply["code"], "forbidden", "{reply}");
    }
    let erin_now = request(
        &mut admin,
        serde_json::json!({"type": "get_user", "id": 2, "username": "erin"}),
    )
    .await;
    assert_eq!(erin_now["user"]["role"]["kind"], "user");
}

/// Checks that revoking a grant ends the user's live subscription to it
/// with an `unsubscribed` frame that carries no id, and that later
/// messages on the topic don't reach it.
#[tokio::test]
async fn revoking_a_grant_ends_live_subscriptions() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 1, "username": "gina", "password": "gina-password", "role": {"kind": "readonly", "topics": [{"pattern": "news/#", "access": "subscribe"}, {"pattern": "own/#", "access": "subscribe"}]}}),
    )
    .await;
    let (mut gina, _) = user_connection(addr, "gina", "gina-password").await;
    let subscribed = request(
        &mut gina,
        serde_json::json!({"type": "subscribe", "id": 1, "topic": "news/today"}),
    )
    .await;
    assert_eq!(subscribed["type"], "subscribed", "{subscribed}");

    let revoked = request(
        &mut admin,
        serde_json::json!({"type": "revoke", "id": 2, "username": "gina", "pattern": "news/#"}),
    )
    .await;
    assert_eq!(revoked["type"], "user", "{revoked}");
    let notice: serde_json::Value =
        serde_json::from_str(&next_text(&mut gina).await).expect("JSON");
    assert_eq!(
        notice,
        serde_json::json!({"type": "unsubscribed", "topic": "news/today"})
    );

    send_json(
        &mut admin,
        serde_json::json!({"type": "publish", "topic": "news/today", "payload": "missed"}),
    )
    .await;
    let again = request(
        &mut gina,
        serde_json::json!({"type": "subscribe", "id": 3, "topic": "news/today"}),
    )
    .await;
    assert_eq!(again["code"], "not_authorized", "{again}");
    let missing = request(
        &mut admin,
        serde_json::json!({"type": "revoke", "id": 4, "username": "gina", "pattern": "news/#"}),
    )
    .await;
    assert_eq!(missing["code"], "bad_request", "{missing}");
}

/// Checks that disabling an account closes its connections with
/// `account_disabled`, refuses its password and its outstanding token, and
/// that enabling it again restores access.
#[tokio::test]
async fn disabling_an_account_closes_it_and_keeps_it_out() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 1, "username": "hank", "password": "hank-password", "role": {"kind": "user"}}),
    )
    .await;
    let (mut hank, token) = user_connection(addr, "hank", "hank-password").await;
    let disabled = request(
        &mut admin,
        serde_json::json!({"type": "set_enabled", "id": 2, "username": "hank", "enabled": false}),
    )
    .await;
    assert_eq!(disabled["user"]["enabled"], false, "{disabled}");
    assert_eq!(
        next_close(&mut hank, Duration::from_secs(2)).await,
        (
            CloseCode::AccountDisabled.code(),
            "account_disabled".to_string()
        )
    );

    let mut retry = raw_connection(addr).await;
    let login = send_raw(
        &mut retry,
        r#"{"type": "login", "username": "hank", "password": "hank-password"}"#,
    )
    .await;
    assert_eq!(login["type"], "error", "{login}");
    let reauth = send_raw(
        &mut retry,
        &serde_json::json!({"type": "auth", "token": token}).to_string(),
    )
    .await;
    assert_eq!(reauth["code"], "auth_failed", "{reauth}");
    assert_eq!(
        next_close(&mut retry, Duration::from_secs(2)).await.0,
        CloseCode::AccountDisabled.code()
    );

    request(
        &mut admin,
        serde_json::json!({"type": "set_enabled", "id": 3, "username": "hank", "enabled": true}),
    )
    .await;
    user_connection(addr, "hank", "hank-password").await;
}

/// Checks that deleting an account closes its connections and removes it,
/// and that deleting or reading a missing account gets `user_not_found`.
#[tokio::test]
async fn deleting_an_account_removes_it() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 1, "username": "ivan", "password": "ivan-password", "role": {"kind": "user"}}),
    )
    .await;
    let (mut ivan, _) = user_connection(addr, "ivan", "ivan-password").await;
    let deleted = request(
        &mut admin,
        serde_json::json!({"type": "delete_user", "id": 2, "username": "ivan"}),
    )
    .await;
    assert_eq!(
        deleted,
        serde_json::json!({"type": "user_deleted", "id": 2, "username": "ivan"})
    );
    assert_eq!(
        next_close(&mut ivan, Duration::from_secs(2)).await.0,
        CloseCode::AccountDisabled.code()
    );
    for (id, kind) in [(3, "get_user"), (4, "delete_user"), (5, "set_password")] {
        let reply = request(
            &mut admin,
            serde_json::json!({"type": kind, "id": id, "username": "ivan", "password": "new-password"}),
        )
        .await;
        assert_eq!(reply["code"], "user_not_found", "{reply}");
    }
}

/// Checks that the last enabled admin can't be disabled, deleted or
/// demoted, and that it can once there is another admin.
#[tokio::test]
async fn the_last_admin_is_kept() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    for (id, attempt) in [
        serde_json::json!({"type": "set_enabled", "username": "admin", "enabled": false}),
        serde_json::json!({"type": "delete_user", "username": "admin"}),
        serde_json::json!({"type": "set_role", "username": "admin", "role": {"kind": "user"}}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut attempt = attempt;
        attempt["id"] = serde_json::json!(id);
        let reply = request(&mut admin, attempt).await;
        assert_eq!(reply["code"], "bad_request", "{reply}");
    }
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 10, "username": "judy", "password": "judy-password", "role": {"kind": "admin"}}),
    )
    .await;
    let demoted = request(
        &mut admin,
        serde_json::json!({"type": "set_role", "id": 11, "username": "admin", "role": {"kind": "readonly"}}),
    )
    .await;
    assert_eq!(demoted["user"]["role"]["kind"], "readonly", "{demoted}");
    let after = request(
        &mut admin,
        serde_json::json!({"type": "list_users", "id": 12}),
    )
    .await;
    assert_eq!(after["code"], "forbidden", "{after}");
}

/// Checks the input errors: invalid usernames, passwords and patterns get
/// `bad_request`, and an existing name gets `user_exists`.
#[tokio::test]
async fn bad_admin_requests_are_refused() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    let cases = [
        (
            serde_json::json!({"type": "create_user", "username": "admin", "password": "another-password", "role": {"kind": "user"}}),
            "user_exists",
        ),
        (
            serde_json::json!({"type": "create_user", "username": "a b", "password": "a-fine-password", "role": {"kind": "user"}}),
            "bad_request",
        ),
        (
            serde_json::json!({"type": "create_user", "username": "kim", "password": "x", "role": {"kind": "user"}}),
            "bad_request",
        ),
        (
            serde_json::json!({"type": "create_user", "username": "kim", "password": "a-fine-password", "role": {"kind": "user", "topics": [{"pattern": "a/#/b", "access": "both"}]}}),
            "bad_request",
        ),
        (
            serde_json::json!({"type": "create_user", "username": "kim", "password": "a-fine-password", "role": {"kind": "readonly", "topics": [{"pattern": "a", "access": "publish"}]}}),
            "bad_request",
        ),
        (
            serde_json::json!({"type": "grant", "username": "admin", "pattern": "a", "access": "both"}),
            "bad_request",
        ),
        (
            serde_json::json!({"type": "grant", "username": "nobody", "pattern": "a", "access": "both"}),
            "user_not_found",
        ),
    ];
    for (id, (attempt, code)) in cases.into_iter().enumerate() {
        let mut attempt = attempt;
        attempt["id"] = serde_json::json!(id);
        let reply = request(&mut admin, attempt).await;
        assert_eq!(reply["code"], code, "{reply}");
    }
}

/// Starts a server on a free port, letting the test adjust it first.
///
/// Args:
///     settings: The settings to configure the server from.
///     configure: Changes to make to the server before it serves.
///
/// Returns:
///     The address to connect to.
async fn start_configured(
    settings: &Settings,
    configure: impl FnOnce(Server) -> Server,
) -> std::net::SocketAddr {
    let broker = SharedBroker::new(
        Broker::new(settings.broker.clone()),
        Arc::new(MemoryStorage::new()),
        LogSettings {
            durability: Durability::None,
            retention: settings.storage.retention.to_retention(),
            session_expiry: Duration::from_secs(settings.storage.session_expiry_secs),
        },
    )
    .expect("memory storage accepts durability none");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let server = configure(Server::from_settings(broker, &settings.server).await);
    tokio::spawn(server.serve(listener));
    addr
}

/// Connects with an `X-Forwarded-For` header, as a proxy would.
///
/// Args:
///     addr: The server's address.
///     forwarded_for: The header's value.
///
/// Returns:
///     The connection.
async fn forwarded_connection(addr: std::net::SocketAddr, forwarded_for: &str) -> TestSocket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = "ws://localhost/".into_client_request().expect("request");
    request.headers_mut().insert(
        "x-forwarded-for",
        forwarded_for.parse().expect("header value"),
    );
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    tokio_tungstenite::client_async(request, stream)
        .await
        .expect("handshake")
        .0
}

/// Rate limiters that lock an address out after two failed logins.
///
/// Returns:
///     The limiters.
fn two_strikes() -> RateLimiters {
    RateLimiters::from_settings(&popsub_config::LimitSettings {
        address_login_attempts: 2,
        ..Default::default()
    })
}

/// Fails two logins and then tries the right password, all from one connection.
///
/// Args:
///     ws: The connection.
///
/// Returns:
///     The reply to the third, correct login.
async fn two_bad_logins_then_a_good_one(ws: &mut TestSocket) -> serde_json::Value {
    for _ in 0..2 {
        let reply = send_raw(
            ws,
            r#"{"type": "login", "username": "admin", "password": "wrong-password"}"#,
        )
        .await;
        assert_eq!(reply["code"], "invalid_credentials", "{reply}");
    }
    send_raw(
        ws,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await
}

/// Checks that behind a trusted proxy the login limits key on the forwarded
/// address: locking out one client leaves another, through the same proxy, free.
#[tokio::test]
async fn trusted_proxies_key_limits_on_the_forwarded_address() {
    let settings = Settings::default();
    let proxies = crate::TrustedProxies::parse(&["127.0.0.1"]).expect("parse");
    let addr = start_configured(&settings, |server| {
        server
            .with_rate_limiters(two_strikes())
            .with_trusted_proxies(proxies)
    })
    .await;
    let mut guesser = forwarded_connection(addr, "203.0.113.1").await;
    let locked = two_bad_logins_then_a_good_one(&mut guesser).await;
    assert_eq!(locked["code"], "locked_out", "{locked}");
    let mut other = forwarded_connection(addr, "198.51.100.2, 127.0.0.1").await;
    let login = send_raw(
        &mut other,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await;
    assert_eq!(login["type"], "logged_in", "{login}");
}

/// Checks that without trusted proxies the header is ignored: clients
/// claiming different addresses from one socket address share its limits.
#[tokio::test]
async fn forwarded_addresses_from_untrusted_peers_are_ignored() {
    let settings = Settings::default();
    let addr = start_configured(&settings, |server| server.with_rate_limiters(two_strikes())).await;
    let mut guesser = forwarded_connection(addr, "203.0.113.1").await;
    two_bad_logins_then_a_good_one(&mut guesser).await;
    let mut pretender = forwarded_connection(addr, "198.51.100.2").await;
    let login = send_raw(
        &mut pretender,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await;
    assert_eq!(login["code"], "locked_out", "{login}");
}

/// Checks that a connection over `broker.max_connections` is closed with
/// `try_again_later`, and that a slot freed by a closed connection can be used again.
#[tokio::test]
async fn connections_over_the_limit_are_told_to_try_again_later() {
    let mut settings = Settings::default();
    settings.broker.max_connections = 2;
    let addr = start_configured(&settings, |server| server).await;
    let mut first = admin_connection(addr, &settings).await;
    let _second = admin_connection(addr, &settings).await;
    let mut third = raw_connection(addr).await;
    assert_eq!(
        next_close(&mut third, Duration::from_secs(2)).await,
        (
            CloseCode::TryAgainLater.code(),
            "try_again_later".to_string()
        )
    );
    first.close(None).await.expect("close");
    drop(first);
    let mut admitted = false;
    for _ in 0..100 {
        let mut retry = raw_connection(addr).await;
        let reply = tokio::time::timeout(
            Duration::from_secs(2),
            retry.send(WsMessage::Text(
                r#"{"type": "login", "username": "admin", "password": "password"}"#.into(),
            )),
        )
        .await;
        if reply.is_ok()
            && let Ok(Some(Ok(WsMessage::Text(text)))) =
                tokio::time::timeout(Duration::from_secs(2), retry.next()).await
            && text.contains("logged_in")
        {
            admitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(admitted, "a freed slot was never reused");
}

/// Checks that a socket that never sends a WebSocket handshake is dropped
/// after the handshake timeout, so it can't hold a connection slot.
#[tokio::test]
async fn silent_sockets_are_dropped_after_the_handshake_timeout() {
    use tokio::io::AsyncReadExt;
    let settings = Settings::default();
    let addr = start_configured(&settings, |server| {
        server.with_handshake_timeout(Duration::from_millis(200))
    })
    .await;
    let mut silent = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut buffer = [0u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(2), silent.read(&mut buffer))
        .await
        .expect("the server hung up within two seconds");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
}

/// Checks that the `[limits]` settings reach the limiters, with values
/// below 1 raised to 1 and idle times long enough for the longest lockout.
#[test]
fn rate_limits_come_from_the_settings() {
    let limiters = RateLimiters::from_settings(&popsub_config::LimitSettings {
        login_attempts: 0,
        max_lockout_secs: 7200,
        publish_per_second: 1,
        publish_burst: 1200,
        ..Default::default()
    });
    limiters.login_limiter.record_failure("a|b");
    assert!(limiters.login_limiter.is_locked("a|b"));
    assert_eq!(limiters.login_idle, Duration::from_secs(7200));
    assert_eq!(limiters.message_idle, Duration::from_secs(1200));
    assert_eq!(
        RateLimiters::default().login_idle,
        Duration::from_secs(3600)
    );
}

/// Checks that one client address can't hold more than
/// `server.max_connections_per_address` connections, while others still can,
/// and that the count is per forwarded address behind a trusted proxy.
#[tokio::test]
async fn one_address_cannot_take_every_connection() {
    let mut settings = Settings::default();
    settings.server.max_connections_per_address = 2;
    let proxies = crate::TrustedProxies::parse(&["127.0.0.1"]).expect("parse");
    let addr = start_configured(&settings, |server| server.with_trusted_proxies(proxies)).await;
    let mut first = forwarded_connection(addr, "203.0.113.1").await;
    let _second = forwarded_connection(addr, "203.0.113.1").await;
    let mut third = forwarded_connection(addr, "203.0.113.1").await;
    assert_eq!(
        next_close(&mut third, Duration::from_secs(2)).await.0,
        CloseCode::TryAgainLater.code()
    );
    let mut elsewhere = forwarded_connection(addr, "198.51.100.2").await;
    let login = send_raw(
        &mut elsewhere,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await;
    assert_eq!(login["type"], "logged_in", "{login}");
    first.close(None).await.expect("close");
    drop(first);
    let mut admitted = false;
    for _ in 0..100 {
        let mut retry = forwarded_connection(addr, "203.0.113.1").await;
        let sent = retry
            .send(WsMessage::Text(
                r#"{"type": "login", "username": "admin", "password": "password"}"#.into(),
            ))
            .await;
        if sent.is_ok()
            && let Ok(Some(Ok(WsMessage::Text(text)))) =
                tokio::time::timeout(Duration::from_secs(2), retry.next()).await
            && text.contains("logged_in")
        {
            admitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(admitted, "the address's freed slot was never reused");
}

/// A self-signed certificate for `127.0.0.1`, as PEM.
///
/// Returns:
///     The certificate and its private key.
fn self_signed() -> (String, String) {
    let generated =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).expect("certificate");
    (generated.cert.pem(), generated.key_pair.serialize_pem())
}

/// Opens a `wss://` connection that trusts one certificate.
///
/// Args:
///     addr: The server's address.
///     trusted: The certificate to trust, as PEM.
///
/// Returns:
///     The connection, or the error from the TLS or WebSocket handshake.
async fn tls_connection(
    addr: std::net::SocketAddr,
    trusted: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    String,
> {
    use rustls::pki_types::pem::PemObject;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(trusted.as_bytes()) {
        roots.add(certificate.expect("PEM")).expect("root");
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let name = rustls::pki_types::ServerName::try_from("127.0.0.1").expect("name");
    let tls = connector
        .connect(name, stream)
        .await
        .map_err(|e| e.to_string())?;
    tokio_tungstenite::client_async(format!("wss://{addr}/"), tls)
        .await
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

/// Checks that a server with a certificate serves `wss://`: a client that
/// trusts it logs in, one that doesn't fails the handshake, and a plain
/// `ws://` client is refused.
#[tokio::test]
async fn tls_servers_serve_wss_only() {
    let settings = Settings::default();
    let (cert, key) = self_signed();
    let tls = crate::ServerTls::from_pem(cert.as_bytes(), key.as_bytes()).expect("tls");
    let addr = start_configured(&settings, |server| server.with_tls(tls)).await;

    let mut ws = tls_connection(addr, &cert).await.expect("wss handshake");
    ws.send(WsMessage::Text(
        r#"{"type": "login", "username": "admin", "password": "password"}"#.into(),
    ))
    .await
    .expect("send");
    let reply = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("a reply")
        .expect("open")
        .expect("read");
    assert!(reply.to_text().expect("text").contains("logged_in"));

    let (other_cert, _) = self_signed();
    assert!(tls_connection(addr, &other_cert).await.is_err());

    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let plain = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::client_async("ws://localhost/", stream),
    )
    .await
    .expect("the server answers a plain client in time");
    assert!(plain.is_err(), "a plain ws:// client was served");
}

/// Checks that a certificate without its key, or with another key, is refused.
#[test]
fn mismatched_tls_material_is_refused() {
    let (cert, key) = self_signed();
    let (_, other_key) = self_signed();
    assert!(crate::ServerTls::from_pem(cert.as_bytes(), key.as_bytes()).is_ok());
    assert!(crate::ServerTls::from_pem(cert.as_bytes(), other_key.as_bytes()).is_err());
    assert!(crate::ServerTls::from_pem(b"", key.as_bytes()).is_err());
    assert!(crate::ServerTls::from_pem(cert.as_bytes(), b"").is_err());
}

/// Connects, sends a PROXY protocol header, then does the WebSocket handshake.
///
/// Args:
///     addr: The server's address.
///     header: The PROXY header bytes.
///
/// Returns:
///     The connection, or the handshake error.
async fn proxied_connection(
    addr: std::net::SocketAddr,
    header: &[u8],
) -> Result<TestSocket, String> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    stream.write_all(header).await.expect("write the header");
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::client_async("ws://localhost/", stream),
    )
    .await
    .expect("the server answers in time")
    .map(|(ws, _)| ws)
    .map_err(|e| e.to_string())
}

/// A version 2 PROXY header for an IPv4 client.
///
/// Args:
///     client: The client's address.
///
/// Returns:
///     The header bytes.
fn proxy_v2(client: std::net::Ipv4Addr) -> Vec<u8> {
    let mut header = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    header.extend([0x21, 0x11, 0, 12]);
    header.extend(client.octets());
    header.extend([127, 0, 0, 1, 0xc8, 0x22, 0x1f, 0x90]);
    header
}

/// Checks that with `proxy_protocol` on, the login limits key on the
/// address from the PROXY header (version 1 or 2), and that a trusted peer
/// sending no header is dropped.
#[tokio::test]
async fn proxy_protocol_headers_give_the_client_address() {
    let mut settings = Settings::default();
    settings.server.proxy_protocol = true;
    let proxies = crate::TrustedProxies::parse(&["127.0.0.1"]).expect("parse");
    let addr = start_configured(&settings, |server| {
        server
            .with_rate_limiters(two_strikes())
            .with_trusted_proxies(proxies)
    })
    .await;
    let mut guesser = proxied_connection(addr, b"PROXY TCP4 203.0.113.1 127.0.0.1 51234 8080\r\n")
        .await
        .expect("v1 handshake");
    let locked = two_bad_logins_then_a_good_one(&mut guesser).await;
    assert_eq!(locked["code"], "locked_out", "{locked}");

    let mut same = proxied_connection(addr, &proxy_v2("203.0.113.1".parse().expect("ip")))
        .await
        .expect("v2 handshake");
    let login = send_raw(
        &mut same,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await;
    assert_eq!(login["code"], "locked_out", "{login}");

    let mut other = proxied_connection(addr, &proxy_v2("198.51.100.2".parse().expect("ip")))
        .await
        .expect("v2 handshake");
    let login = send_raw(
        &mut other,
        r#"{"type": "login", "username": "admin", "password": "password"}"#,
    )
    .await;
    assert_eq!(login["type"], "logged_in", "{login}");

    assert!(proxied_connection(addr, b"").await.is_err());
}

/// Checks that `ping` is answered with `pong` carrying its id, before and
/// after authenticating, and that the connection stays open.
#[tokio::test]
async fn pings_are_answered_before_and_after_auth() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut ws = raw_connection(addr).await;
    let early = send_raw(&mut ws, r#"{"type": "ping", "id": "a"}"#).await;
    assert_eq!(early, serde_json::json!({"type": "pong", "id": "a"}));
    let mut admin = admin_connection(addr, &settings).await;
    let late = send_raw(&mut admin, r#"{"type": "ping", "id": 2}"#).await;
    assert_eq!(late, serde_json::json!({"type": "pong", "id": 2}));
}

/// Checks that changing a password cancels the account's tokens: its live
/// connection is closed with `token_expired`, the old token is refused at
/// `auth`, the old password no longer logs in, and the new one does.
#[tokio::test]
async fn a_password_change_cancels_tokens_and_connections() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 1, "username": "pat", "password": "pat-password-1", "role": {"kind": "user"}}),
    )
    .await;
    let (mut pat, old_token) = user_connection(addr, "pat", "pat-password-1").await;
    let changed = request(
        &mut admin,
        serde_json::json!({"type": "set_password", "id": 2, "username": "pat", "password": "pat-password-2"}),
    )
    .await;
    assert_eq!(changed["type"], "user", "{changed}");
    assert_eq!(
        next_close(&mut pat, Duration::from_secs(2)).await.0,
        CloseCode::TokenExpired.code()
    );

    let mut stale = raw_connection(addr).await;
    let refused = send_raw(
        &mut stale,
        &serde_json::json!({"type": "auth", "token": old_token}).to_string(),
    )
    .await;
    assert_eq!(refused["code"], "auth_failed", "{refused}");
    assert_eq!(
        next_close(&mut stale, Duration::from_secs(2)).await.0,
        CloseCode::AuthFailed.code()
    );

    let mut retry = raw_connection(addr).await;
    let old_login = send_raw(
        &mut retry,
        r#"{"type": "login", "username": "pat", "password": "pat-password-1"}"#,
    )
    .await;
    assert_eq!(old_login["code"], "invalid_credentials", "{old_login}");
    user_connection(addr, "pat", "pat-password-2").await;
}

/// Checks the topic admin requests: listing and describing topics, purging
/// one (its numbering carries on and a subscriber that was behind is told
/// with `truncated`), `topic_not_found`, and `forbidden` for non-admins.
#[tokio::test]
async fn admins_list_describe_and_purge_topics() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    for (topic, count) in [("orders", 3), ("audit", 1)] {
        for n in 0..count {
            let published = request(
                &mut admin,
                serde_json::json!({"type": "publish", "id": n, "topic": topic, "payload": "x", "qos": 1}),
            )
            .await;
            assert_eq!(published["type"], "published", "{published}");
        }
    }
    let listed = request(
        &mut admin,
        serde_json::json!({"type": "list_topics", "id": 10}),
    )
    .await;
    assert_eq!(
        listed["topics"],
        serde_json::json!([
            {"name": "audit", "first_seq": 1, "last_seq": 1, "messages": 1, "subscribers": 0},
            {"name": "orders", "first_seq": 1, "last_seq": 3, "messages": 3, "subscribers": 0},
        ])
    );
    let purged = request(
        &mut admin,
        serde_json::json!({"type": "purge_topic", "id": 11, "topic": "orders"}),
    )
    .await;
    assert_eq!(
        purged["topic"],
        serde_json::json!({"name": "orders", "first_seq": 4, "last_seq": 3, "messages": 0, "subscribers": 0})
    );
    let next = request(
        &mut admin,
        serde_json::json!({"type": "publish", "id": 12, "topic": "orders", "payload": "y", "qos": 1}),
    )
    .await;
    assert_eq!(next["seq"], 4, "{next}");
    send_json(
        &mut admin,
        serde_json::json!({"type": "subscribe", "topic": "orders", "from": {"seq": 1}}),
    )
    .await;
    let mut kinds = Vec::new();
    while kinds.len() < 3 {
        let frame: serde_json::Value =
            serde_json::from_str(&next_text(&mut admin).await).expect("JSON");
        kinds.push((
            frame["type"].clone(),
            frame["first_available"].clone(),
            frame["seq"].clone(),
        ));
    }
    assert_eq!(kinds[1].0, "truncated");
    assert_eq!(kinds[1].1, 4);
    assert_eq!(
        (kinds[2].0.clone(), kinds[2].2.clone()),
        (serde_json::json!("message"), serde_json::json!(4))
    );
    let described = request(
        &mut admin,
        serde_json::json!({"type": "get_topic", "id": 13, "topic": "orders"}),
    )
    .await;
    assert_eq!(described["topic"]["subscribers"], 1, "{described}");
    let missing = request(
        &mut admin,
        serde_json::json!({"type": "get_topic", "id": 14, "topic": "nothing-here"}),
    )
    .await;
    assert_eq!(missing["code"], "topic_not_found", "{missing}");

    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 15, "username": "tess", "password": "tess-password-1", "role": {"kind": "user"}}),
    )
    .await;
    let (mut tess, _) = user_connection(addr, "tess", "tess-password-1").await;
    let refused = request(
        &mut tess,
        serde_json::json!({"type": "purge_topic", "id": 1, "topic": "orders"}),
    )
    .await;
    assert_eq!(refused["code"], "forbidden", "{refused}");
}

/// Reads frames until a number of them have arrived, failing after two seconds each.
///
/// Args:
///     ws: The connection.
///     count: How many frames to read.
///
/// Returns:
///     The frames as JSON, in order.
async fn frames_json(ws: &mut TestSocket, count: usize) -> Vec<serde_json::Value> {
    let mut frames = Vec::new();
    while frames.len() < count {
        frames.push(serde_json::from_str(&next_text(ws).await).expect("JSON"));
    }
    frames
}

/// Checks that nothing arrives on a connection for a while.
///
/// Args:
///     ws: The connection.
///
/// Returns:
///     The first text frame that did arrive, if any.
async fn nothing_arrives(ws: &mut TestSocket) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(WsMessage::Text(text)))) => return Some(text.to_string()),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

/// Checks wildcard subscriptions: matching topics the user may read are
/// joined with the requested replay, a topic created later is joined from
/// its first message, topics the user may not read are never joined, and
/// unsubscribing the pattern ends the topics it brought in.
#[tokio::test]
async fn pattern_subscriptions_join_matching_topics() {
    let settings = Settings::default();
    let (addr, _broker) = start_server(&settings, RateLimiters::default()).await;
    let mut admin = admin_connection(addr, &settings).await;
    let publish = async |admin: &mut TestSocket, topic: &str, payload: &str| {
        let published = request(
            admin,
            serde_json::json!({"type": "publish", "id": 1, "topic": topic, "payload": payload, "qos": 1}),
        )
        .await;
        assert_eq!(published["type"], "published", "{published}");
    };
    publish(&mut admin, "orders/a", "a1").await;
    publish(&mut admin, "orders/b", "b1").await;
    publish(&mut admin, "other/x", "x1").await;
    request(
        &mut admin,
        serde_json::json!({"type": "create_user", "id": 2, "username": "wren", "password": "wren-password-1", "role": {"kind": "readonly", "topics": [{"pattern": "orders/#", "access": "subscribe"}]}}),
    )
    .await;
    let (mut wren, _) = user_connection(addr, "wren", "wren-password-1").await;

    let bad = request(
        &mut wren,
        serde_json::json!({"type": "subscribe", "id": 1, "topic": "orders/#/x"}),
    )
    .await;
    assert_eq!(bad["code"], "invalid_topic", "{bad}");

    send_json(
        &mut wren,
        serde_json::json!({"type": "subscribe", "id": 2, "topic": "#", "from": {"last": 5}}),
    )
    .await;
    let frames = frames_json(&mut wren, 5).await;
    assert_eq!(
        frames[0],
        serde_json::json!({"type": "subscribed", "id": 2, "topic": "#", "replay_from": 0, "replay_to": 0})
    );
    let summary: Vec<(String, String)> = frames[1..]
        .iter()
        .map(|f| {
            (
                f["type"].as_str().unwrap_or_default().to_string(),
                f["topic"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("subscribed".to_string(), "orders/a".to_string()),
            ("message".to_string(), "orders/a".to_string()),
            ("subscribed".to_string(), "orders/b".to_string()),
            ("message".to_string(), "orders/b".to_string()),
        ]
    );
    assert!(frames[1].get("id").is_none(), "{}", frames[1]);

    publish(&mut admin, "orders/c", "c1").await;
    let joined = frames_json(&mut wren, 2).await;
    assert_eq!(
        joined[0],
        serde_json::json!({"type": "subscribed", "topic": "orders/c", "replay_from": 1, "replay_to": 1})
    );
    assert_eq!(
        (
            joined[1]["topic"].clone(),
            joined[1]["seq"].clone(),
            joined[1]["payload"].clone()
        ),
        (
            serde_json::json!("orders/c"),
            serde_json::json!(1),
            serde_json::json!("c1")
        )
    );
    publish(&mut admin, "secret/z", "z1").await;
    publish(&mut admin, "other/x", "x2").await;
    assert_eq!(nothing_arrives(&mut wren).await, None);

    let left = request(
        &mut wren,
        serde_json::json!({"type": "unsubscribe", "id": 3, "topic": "#"}),
    )
    .await;
    assert_eq!(
        left,
        serde_json::json!({"type": "unsubscribed", "id": 3, "topic": "#"})
    );
    publish(&mut admin, "orders/a", "a2").await;
    publish(&mut admin, "orders/d", "d1").await;
    assert_eq!(nothing_arrives(&mut wren).await, None);
}
