//! End-to-end tests that start a real server on a free port and talk to it
//! over a WebSocket.
//!
//! Each test gets its own server, broker and in-memory storage, configured
//! from `Settings::default()` unless it says otherwise.

use crate::websocket::{RateLimiters, Server};
use futures_util::{SinkExt, StreamExt};
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::Settings;
use popsub_protocol::{ClientMessage, ErrorCode, ServerMessage, StartFrom};
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
        },
    )
    .await;

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
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("no frame within two seconds")
        .expect("connection closed")
        .expect("read failed");
    serde_json::from_slice(&frame.into_data()).expect("server frames parse")
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
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("no reply within two seconds")
        .expect("connection closed")
        .expect("read failed");
    serde_json::from_slice(&frame.into_data()).expect("replies are JSON")
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
    assert!(wait_for_no_clients(&broker).await);
}
