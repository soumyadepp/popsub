//! Tests for the client against a real PopSub server started in the test.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::Settings;
use popsub_protocol::{CloseCode, ErrorCode, StartFrom};
use popsub_storage::{Durability, FileStorage, MemoryStorage, Storage};
use popsub_transport::Server;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::{
    AckMode, Client, ClientConfig, Credentials, Error, Event, ReconnectPolicy, Subscription,
};

/// A server running in the test.
struct TestServer {
    /// Where it listens.
    addr: SocketAddr,
    /// Its broker, for checking server-side state.
    broker: SharedBroker,
    /// Its accept loop, aborted to stop it.
    accepting: JoinHandle<()>,
}

impl TestServer {
    /// Starts a server on a port.
    ///
    /// Args:
    ///     settings: Its settings.
    ///     storage: Where it keeps topic logs and sessions.
    ///     port: The port, or 0 for any free one.
    ///
    /// Returns:
    ///     The running server.
    async fn start(settings: &Settings, storage: Arc<dyn Storage>, port: u16) -> TestServer {
        let broker = SharedBroker::new(
            Broker::new(settings.broker.clone()),
            storage,
            LogSettings {
                durability: Durability::None,
                retention: settings.storage.retention.to_retention(),
                session_expiry: Duration::from_secs(3600),
            },
        )
        .expect("the backend accepts durability none");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = Server::from_settings(broker.clone(), &settings.server).await;
        let accepting = tokio::spawn(server.serve(listener));
        TestServer {
            addr,
            broker,
            accepting,
        }
    }

    /// Starts a server on a free port with in-memory storage.
    ///
    /// Args:
    ///     settings: Its settings.
    ///
    /// Returns:
    ///     The running server.
    async fn memory(settings: &Settings) -> TestServer {
        Self::start(settings, Arc::new(MemoryStorage::new()), 0).await
    }

    /// The configuration for the default admin, reconnecting quickly.
    ///
    /// Returns:
    ///     The configuration.
    fn config(&self) -> ClientConfig {
        ClientConfig::new(
            format!("ws://{}", self.addr),
            Credentials::password("admin", "password"),
        )
        .reconnect(ReconnectPolicy {
            initial: Duration::from_millis(20),
            max: Duration::from_millis(200),
            max_attempts: None,
        })
        .request_timeout(Duration::from_secs(10))
    }

    /// Whether every subscription on a topic has nothing unacknowledged.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     True when nothing is in flight.
    fn nothing_in_flight(&self, topic: &str) -> bool {
        self.broker.with(|b| {
            b.topics
                .get(topic)
                .is_none_or(|t| t.subscribers.values().all(|s| s.inflight.is_empty()))
        })
    }
}

/// Waits up to two seconds for a condition.
///
/// Args:
///     check: The condition.
///
/// Returns:
///     Whether it became true in time.
async fn eventually(check: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Reads the next message, failing the test after five seconds.
///
/// Args:
///     subscription: The subscription.
///
/// Returns:
///     The message's `seq` and payload, and the message.
async fn next(subscription: &mut Subscription) -> crate::Message {
    tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("a message within five seconds")
        .expect("the subscription is open")
}

/// Waits for an event matching a condition, failing after five seconds.
///
/// Args:
///     events: The event receiver.
///     wanted: The condition.
///
/// Returns:
///     The matching event.
async fn wait_for_event(
    events: &mut broadcast::Receiver<Event>,
    wanted: impl Fn(&Event) -> bool,
) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("events");
            if wanted(&event) {
                return event;
            }
        }
    })
    .await
    .expect("the event within five seconds")
}

/// Checks that a confirmed publish returns its seq, the subscriber receives
/// it, and automatic acknowledgement clears it on the server.
#[tokio::test]
async fn publish_confirmed_and_receive_with_auto_ack() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config()).await.expect("connect");
    let mut chat = client
        .subscribe("chat", StartFrom::New)
        .await
        .expect("subscribe");
    assert_eq!(chat.topic(), "chat");
    let stored = client
        .publish_confirmed("chat", "hello")
        .await
        .expect("publish");
    assert_eq!(stored.seq, 1);
    let message = next(&mut chat).await;
    assert_eq!(
        (message.seq, message.payload.as_str(), message.qos),
        (1, "hello", 1)
    );
    assert_eq!(message.message_id, stored.message_id);
    assert!(eventually(|| server.nothing_in_flight("chat")).await);
    client.publish("chat", "fire and forget").expect("publish");
    assert_eq!(next(&mut chat).await.seq, 2);
}

/// Checks that in manual mode a message stays unacknowledged until `ack`.
#[tokio::test]
async fn manual_ack_clears_the_message_on_the_server() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config().ack_mode(AckMode::Manual))
        .await
        .expect("connect");
    let mut chat = client
        .subscribe("chat", StartFrom::New)
        .await
        .expect("subscribe");
    client
        .publish_confirmed("chat", "hello")
        .await
        .expect("publish");
    let message = next(&mut chat).await;
    assert!(!server.nothing_in_flight("chat"));
    message.ack();
    assert!(eventually(|| server.nothing_in_flight("chat")).await);
}

/// Checks that the server's refusals come back as typed errors and wrong
/// credentials fail `connect` without retrying forever.
#[tokio::test]
async fn refusals_are_typed_errors() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config()).await.expect("connect");
    let refused = client.subscribe("metrics/#", StartFrom::New).await;
    assert!(
        matches!(
            &refused,
            Err(Error::Server {
                code: ErrorCode::InvalidTopic,
                ..
            })
        ),
        "{refused:?}"
    );
    assert!(!refused.unwrap_err().is_temporary());

    let wrong = ClientConfig {
        credentials: Credentials::password("admin", "wrong"),
        ..server.config()
    };
    let started = std::time::Instant::now();
    let result = Client::connect(wrong).await;
    assert!(
        matches!(
            &result,
            Err(Error::Server {
                code: ErrorCode::InvalidCredentials,
                ..
            })
        ),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// Checks that when the server drops every connection, the client
/// reconnects and an ephemeral subscriber gets everything published while it
/// was away, because it resubscribes from the next `seq` it expects rather
/// than from `new`.
///
/// The missed messages are published on the server itself the moment the
/// connections drop, so they are certainly in before the client is back
/// (reconnecting means logging in again, which takes a password check).
#[tokio::test]
async fn reconnects_and_resubscribes_without_gaps() {
    let server = TestServer::memory(&Settings::default()).await;
    let reader = Client::connect(server.config()).await.expect("connect");
    let mut events = reader.events();
    let mut chat = reader
        .subscribe("chat", StartFrom::New)
        .await
        .expect("subscribe");
    let publish = |payload: &str| popsub_storage::NewRecord {
        message_id: String::new(),
        publisher: "admin".to_string(),
        payload: payload.to_string(),
        qos: 0,
        published_at_ms: 0,
    };
    server
        .broker
        .publish("chat", publish("one"))
        .await
        .expect("publish");
    assert_eq!(next(&mut chat).await.seq, 1);

    assert_eq!(server.broker.close_all(CloseCode::GoingAway), 1);
    server
        .broker
        .publish("chat", publish("two"))
        .await
        .expect("publish");
    server
        .broker
        .publish("chat", publish("three"))
        .await
        .expect("publish");
    let disconnected =
        wait_for_event(&mut events, |e| matches!(e, Event::Disconnected { .. })).await;
    assert_eq!(
        disconnected,
        Event::Disconnected {
            code: Some(CloseCode::GoingAway),
            reconnecting: true
        }
    );
    wait_for_event(&mut events, |e| matches!(e, Event::Connected { .. })).await;
    let seqs = vec![next(&mut chat).await.seq, next(&mut chat).await.seq];
    assert_eq!(seqs, vec![2, 3]);
}

/// Checks that a durable session survives the whole server restarting on
/// the same data directory: the subscription comes back, a message it
/// already received isn't delivered again, and new ones arrive.
#[tokio::test]
async fn a_durable_session_survives_a_server_restart() {
    let settings = Settings::default();
    let dir = tempfile::tempdir().expect("temp dir");
    let first = TestServer::start(
        &settings,
        Arc::new(FileStorage::open(dir.path()).await.expect("storage")),
        0,
    )
    .await;
    let port = first.addr.port();
    let config = first.config().ack_mode(AckMode::Manual).session("phone");
    let reader = Client::connect(config).await.expect("connect");
    let writer = Client::connect(first.config()).await.expect("connect");
    let mut chat = reader
        .subscribe("chat", StartFrom::Resume)
        .await
        .expect("subscribe");
    writer
        .publish_confirmed("chat", "one")
        .await
        .expect("publish");
    writer
        .publish_confirmed("chat", "two")
        .await
        .expect("publish");
    next(&mut chat).await.ack();
    let unacked = next(&mut chat).await;
    assert_eq!(unacked.seq, 2);

    first.accepting.abort();
    first.broker.close_all(CloseCode::GoingAway);
    drop(first);
    let mut storage = None;
    for _ in 0..300 {
        if let Ok(opened) = FileStorage::open(dir.path()).await {
            storage = Some(opened);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let second = TestServer::start(&settings, Arc::new(storage.expect("reopened")), port).await;

    writer
        .publish_confirmed("chat", "three")
        .await
        .expect("publish after restart");
    let after = next(&mut chat).await;
    assert_eq!((after.seq, after.payload.as_str()), (3, "three"));
    unacked.ack();
    after.ack();
    assert!(eventually(|| second.nothing_in_flight("chat")).await);
}

/// Checks that a client whose session is taken over stops for good and
/// says why.
#[tokio::test]
async fn a_taken_over_session_stops_the_client() {
    let server = TestServer::memory(&Settings::default()).await;
    let first = Client::connect(server.config().session("phone"))
        .await
        .expect("connect");
    let mut events = first.events();
    let _second = Client::connect(server.config().session("phone"))
        .await
        .expect("connect");
    let event = wait_for_event(&mut events, |e| matches!(e, Event::Disconnected { .. })).await;
    assert_eq!(
        event,
        Event::Disconnected {
            code: Some(CloseCode::SessionTakenOver),
            reconnecting: false
        }
    );
    assert!(eventually(|| first.stopped().is_some()).await);
    assert_eq!(
        first.publish("chat", "x"),
        Err(Error::Closed(Some(CloseCode::SessionTakenOver)))
    );
}

/// Checks that the server's retries of an unacknowledged message don't
/// reach the application twice.
#[tokio::test]
async fn retries_are_delivered_once() {
    let mut settings = Settings::default();
    settings.broker.ack_timeout_ms = 50;
    let server = TestServer::memory(&settings).await;
    tokio::spawn(server.broker.clone().run_retry_loop());
    let client = Client::connect(server.config().ack_mode(AckMode::Manual))
        .await
        .expect("connect");
    let mut chat = client
        .subscribe("chat", StartFrom::New)
        .await
        .expect("subscribe");
    client
        .publish_confirmed("chat", "once")
        .await
        .expect("publish");
    let message = next(&mut chat).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let again = tokio::time::timeout(Duration::from_millis(200), chat.next()).await;
    assert!(again.is_err(), "a retry reached the application: {again:?}");
    message.ack();
    assert!(eventually(|| server.nothing_in_flight("chat")).await);
}

/// Checks that dropping a subscription unsubscribes it on the server.
#[tokio::test]
async fn dropping_a_subscription_unsubscribes() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config()).await.expect("connect");
    let chat = client
        .subscribe("chat", StartFrom::New)
        .await
        .expect("subscribe");
    assert!(
        server
            .broker
            .with(|b| b.topics["chat"].subscribers.len() == 1)
    );
    drop(chat);
    assert!(
        eventually(|| server
            .broker
            .with(|b| b.topics["chat"].subscribers.is_empty()))
        .await
    );
    client.close();
    assert!(eventually(|| client.stopped().is_some()).await);
}
