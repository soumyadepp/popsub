//! Checks that a real session's log output never contains the usernames,
//! passwords, session ids or client addresses it handled while masking is on.
//!
//! This is its own test binary because it installs the global log
//! subscriber and turns masking on for the whole process.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::Settings;
use popsub_storage::{Durability, MemoryStorage};
use popsub_transport::Server;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Everything logged, shared with the subscriber's writer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    /// Appends a log line.
    ///
    /// Args:
    ///     bytes: The bytes.
    ///
    /// Returns:
    ///     How many were written.
    ///
    /// Raises:
    ///     std::io::Error: Never.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    /// Does nothing; lines are kept in memory.
    ///
    /// Raises:
    ///     std::io::Error: Never.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A client connection in this test.
type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// Sends a frame and reads the next text frame.
///
/// Args:
///     ws: The connection.
///     text: The frame, which need not be valid.
///
/// Returns:
///     The reply.
async fn exchange(ws: &mut Socket, text: &str) -> serde_json::Value {
    ws.send(WsMessage::Text(text.to_string().into()))
        .await
        .expect("send");
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a reply")
            .expect("open")
            .expect("read");
        if let WsMessage::Text(reply) = frame {
            return serde_json::from_str(&reply).expect("JSON");
        }
    }
}

/// Runs registration, a wrong password, a malformed login carrying a
/// password, a login with a durable session, a subscribe, a refused admin
/// request, admin changes (one ending a live subscription), a lookup of a
/// missing user and a lockout, then checks the log for every identifying value.
#[tokio::test(flavor = "multi_thread")]
async fn logs_never_show_identities_while_masking() {
    popsub_utils::pii::set_masking(true);
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();

    let mut settings = Settings::default();
    settings.server.allow_registration = true;
    settings.server.registration_role = "full".to_string();
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    tokio::spawn(
        Server::from_settings(broker, &settings.server)
            .await
            .serve(listener),
    );

    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake");
    let register =
        r#"{"type": "register", "username": "dana.secret", "password": "dana-password-1"}"#;
    assert_eq!(exchange(&mut ws, register).await["type"], "registered");
    let wrong = r#"{"type": "login", "username": "dana.secret", "password": "not-the-password"}"#;
    assert_eq!(exchange(&mut ws, wrong).await["type"], "error");
    let broken =
        r#"{"type": "login", "username": "dana.secret", "password": "hunter2-in-a-broken-frame""#;
    assert_eq!(exchange(&mut ws, broken).await["code"], "bad_request");
    let login = r#"{"type": "login", "username": "dana.secret", "password": "dana-password-1"}"#;
    let token = exchange(&mut ws, login).await["token"].clone();
    let auth = serde_json::json!({"type": "auth", "token": token, "session_id": "danas-laptop"});
    assert_eq!(
        exchange(&mut ws, &auth.to_string()).await["type"],
        "authenticated"
    );
    let subscribe = r#"{"type": "subscribe", "topic": "news"}"#;
    assert_eq!(exchange(&mut ws, subscribe).await["type"], "subscribed");
    let forbidden = r#"{"type": "delete_user", "username": "admin"}"#;
    assert_eq!(exchange(&mut ws, forbidden).await["code"], "forbidden");

    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let (mut admin, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake");
    let admin_login = r#"{"type": "login", "username": "admin", "password": "password"}"#;
    let admin_token = exchange(&mut admin, admin_login).await["token"].clone();
    let admin_auth = serde_json::json!({"type": "auth", "token": admin_token});
    exchange(&mut admin, &admin_auth.to_string()).await;
    let create = r#"{"type": "create_user", "username": "erin.secret", "password": "erin-password-1", "role": {"kind": "user", "topics": [{"pattern": "news", "access": "both"}]}}"#;
    assert_eq!(exchange(&mut admin, create).await["type"], "user");
    let missing = r#"{"type": "get_user", "username": "frank.secret"}"#;
    assert_eq!(
        exchange(&mut admin, missing).await["code"],
        "user_not_found"
    );
    let demote = r#"{"type": "set_role", "username": "dana.secret", "role": {"kind": "readonly"}}"#;
    assert_eq!(exchange(&mut admin, demote).await["type"], "user");

    for _ in 0..6 {
        exchange(
            &mut admin,
            r#"{"type": "login", "username": "erin.secret", "password": "wrong-wrong-wrong"}"#,
        )
        .await;
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    let log = String::from_utf8_lossy(
        &captured
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .to_string();
    assert!(log.contains("masked:"), "nothing was masked:\n{log}");
    assert!(
        log.contains("locked out"),
        "the lockout wasn't logged:\n{log}"
    );
    for secret in [
        "dana.secret",
        "dana-password-1",
        "not-the-password",
        "hunter2",
        "danas-laptop",
        "erin.secret",
        "erin-password-1",
        "frank.secret",
        "127.0.0.1",
    ] {
        assert!(!log.contains(secret), "{secret} reached the log:\n{log}");
    }
}
