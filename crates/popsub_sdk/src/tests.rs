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
    Access, AckMode, Client, ClientConfig, Credentials, Error, Event, ReconnectPolicy, RoleKind,
    RoleSpec, Subscription, TopicGrant,
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
        Self::start_with(settings, storage, port, None).await
    }

    /// Starts a server on a port, serving `wss://` when given a certificate.
    ///
    /// Args:
    ///     settings: Its settings.
    ///     storage: Where it keeps topic logs and sessions.
    ///     port: The port, or 0 for any free one.
    ///     tls: The certificate and key, if any.
    ///
    /// Returns:
    ///     The running server.
    async fn start_with(
        settings: &Settings,
        storage: Arc<dyn Storage>,
        port: u16,
        tls: Option<popsub_transport::ServerTls>,
    ) -> TestServer {
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
        let mut server = Server::from_settings(broker.clone(), &settings.server).await;
        if let Some(tls) = tls {
            server = server.with_tls(tls);
        }
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
    let refused = client.subscribe("metrics/#/x", StartFrom::New).await;
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
        encoding: None,
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

/// Connects as another user, reconnecting quickly.
///
/// Args:
///     server: The server.
///     username: The account.
///     password: Its password.
///
/// Returns:
///     The connected client.
async fn connect_as(server: &TestServer, username: &str, password: &str) -> Client {
    let config = ClientConfig::new(
        format!("ws://{}", server.addr),
        Credentials::password(username, password),
    )
    .reconnect(ReconnectPolicy {
        initial: Duration::from_millis(20),
        max: Duration::from_millis(200),
        max_attempts: None,
    })
    .request_timeout(Duration::from_secs(10));
    Client::connect(config).await.expect("connect")
}

/// Checks the admin calls end to end: creating, reading, listing and
/// granting, `forbidden` for a non-admin, and a revoke ending the user's
/// live subscription with `Event::Unsubscribed`.
#[tokio::test]
async fn admin_calls_manage_accounts_and_revokes_end_subscriptions() {
    let server = TestServer::memory(&Settings::default()).await;
    let admin = Client::connect(server.config()).await.expect("connect");
    let created = admin
        .admin()
        .create_user(
            "lena",
            "lena-password",
            RoleSpec::user(vec![TopicGrant::new("work/#", Access::Subscribe)]),
        )
        .await
        .expect("create");
    assert_eq!(created.username, "lena");
    assert!(created.enabled);
    let granted = admin
        .admin()
        .grant("lena", "chat", Access::Both)
        .await
        .expect("grant");
    assert_eq!(granted.role.topics.len(), 2);
    assert_eq!(admin.admin().get_user("lena").await.expect("get"), granted);
    let names: Vec<String> = admin
        .admin()
        .list_users()
        .await
        .expect("list")
        .into_iter()
        .map(|u| u.username)
        .collect();
    assert_eq!(names, ["admin", "lena"]);

    let lena = connect_as(&server, "lena", "lena-password").await;
    let mut events = lena.events();
    match lena.admin().list_users().await {
        Err(Error::Server { code, .. }) => assert_eq!(code, ErrorCode::Forbidden),
        other => panic!("expected forbidden, got {other:?}"),
    }
    let mut work = lena
        .subscribe("work/today", StartFrom::New)
        .await
        .expect("subscribe");
    admin
        .admin()
        .revoke("lena", "work/#")
        .await
        .expect("revoke");
    assert_eq!(
        wait_for_event(&mut events, |e| matches!(e, Event::Unsubscribed { .. })).await,
        Event::Unsubscribed {
            topic: "work/today".to_string()
        }
    );
    let ended = tokio::time::timeout(Duration::from_secs(5), work.next())
        .await
        .expect("the subscription ends");
    assert!(ended.is_none());
    let mut chat = lena
        .subscribe("chat", StartFrom::New)
        .await
        .expect("chat is still granted");
    admin
        .publish_confirmed("chat", "still here")
        .await
        .expect("publish");
    assert_eq!(next(&mut chat).await.payload, "still here");
}

/// Checks that disabling an account stops its client for good with
/// `AccountDisabled`, and the refusals for missing accounts and the last admin.
#[tokio::test]
async fn disabled_accounts_stop_their_clients() {
    let server = TestServer::memory(&Settings::default()).await;
    let admin = Client::connect(server.config()).await.expect("connect");
    admin
        .admin()
        .create_user("mona", "mona-password", RoleSpec::readonly(["news/#"]))
        .await
        .expect("create");
    let mona = connect_as(&server, "mona", "mona-password").await;
    let disabled = admin
        .admin()
        .set_enabled("mona", false)
        .await
        .expect("disable");
    assert!(!disabled.enabled);
    assert_eq!(disabled.role.kind, RoleKind::Readonly);
    assert!(eventually(|| mona.stopped().is_some()).await);
    assert_eq!(mona.stopped(), Some(Some(CloseCode::AccountDisabled)));

    admin.admin().delete_user("mona").await.expect("delete");
    match admin.admin().get_user("mona").await {
        Err(Error::Server { code, .. }) => assert_eq!(code, ErrorCode::UserNotFound),
        other => panic!("expected user_not_found, got {other:?}"),
    }
    match admin
        .admin()
        .set_role("admin", RoleSpec::readonly(["x"]))
        .await
    {
        Err(Error::Server { code, .. }) => assert_eq!(code, ErrorCode::BadRequest),
        other => panic!("expected bad_request, got {other:?}"),
    }
}

/// Checks that subscribing again straight after dropping a subscription
/// keeps the new one: the reply to the drop's unsubscribe carries an id, so
/// it isn't taken for the server ending the new subscription.
#[tokio::test]
async fn resubscribing_after_a_drop_keeps_the_new_subscription() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config()).await.expect("connect");
    let mut events = client.events();
    for round in 0..20 {
        let old = client
            .subscribe("chat", StartFrom::New)
            .await
            .expect("subscribe");
        drop(old);
        let mut new = client
            .subscribe("chat", StartFrom::New)
            .await
            .expect("subscribe again");
        let payload = format!("round {round}");
        client
            .publish_confirmed("chat", payload.clone())
            .await
            .expect("publish");
        assert_eq!(next(&mut new).await.payload, payload);
    }
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, Event::Unsubscribed { .. }), "{event:?}");
    }
}

/// Checks that the client connects over `wss://` to a server whose
/// self-signed certificate it was told to trust, and publishes and receives.
#[tokio::test]
async fn wss_works_with_an_added_root_certificate() {
    let generated =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).expect("certificate");
    let cert = generated.cert.pem();
    let tls = popsub_transport::ServerTls::from_pem(
        cert.as_bytes(),
        generated.key_pair.serialize_pem().as_bytes(),
    )
    .expect("tls");
    let server = TestServer::start_with(
        &Settings::default(),
        Arc::new(MemoryStorage::new()),
        0,
        Some(tls),
    )
    .await;
    let config = ClientConfig::new(
        format!("wss://{}", server.addr),
        Credentials::password("admin", "password"),
    )
    .add_root_certificates_pem(cert.as_bytes())
    .expect("root");
    let client = Client::connect(config).await.expect("connect over wss");
    let mut chat = client
        .subscribe("chat", StartFrom::New)
        .await
        .expect("subscribe");
    client
        .publish_confirmed("chat", "over tls")
        .await
        .expect("publish");
    assert_eq!(next(&mut chat).await.payload, "over tls");

    let untrusting = ClientConfig::new(
        format!("wss://{}", server.addr),
        Credentials::password("admin", "password"),
    )
    .reconnect(ReconnectPolicy {
        max_attempts: Some(0),
        ..ReconnectPolicy::default()
    });
    match Client::connect(untrusting).await {
        Err(Error::Connect(reason)) => assert!(reason.contains("certificate"), "{reason}"),
        other => panic!("expected a certificate failure, got {other:?}"),
    }
    assert!(matches!(
        ClientConfig::new("wss://x", Credentials::Token("t".to_string()))
            .add_root_certificates_pem(b"not a certificate"),
        Err(Error::Config(_))
    ));
}

/// Checks a wildcard subscription: an existing topic is replayed as asked,
/// topics created later arrive from their first message under their own
/// names, each `(topic, seq)` arrives once across a reconnect, and
/// unsubscribing the pattern stops all of them.
#[tokio::test]
async fn a_wildcard_subscription_follows_matching_topics() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config()).await.expect("connect");
    client
        .publish_confirmed("orders/eu", "old")
        .await
        .expect("publish");
    client
        .publish_confirmed("other", "ignored")
        .await
        .expect("publish");
    let mut orders = client
        .subscribe("orders/#", StartFrom::Last(5))
        .await
        .expect("subscribe");
    assert_eq!(orders.topic(), "orders/#");
    let old = next(&mut orders).await;
    assert_eq!(
        (old.topic.as_str(), old.seq, old.payload.as_str()),
        ("orders/eu", 1, "old")
    );

    client
        .publish_confirmed("orders/us", "first")
        .await
        .expect("publish");
    let first = next(&mut orders).await;
    assert_eq!((first.topic.as_str(), first.seq), ("orders/us", 1));

    let mut events = client.events();
    server.broker.close_all(CloseCode::GoingAway);
    wait_for_event(&mut events, |e| matches!(e, Event::Connected { .. })).await;
    client
        .publish_confirmed("orders/us", "second")
        .await
        .expect("publish");
    client
        .publish_confirmed("orders/asia", "new")
        .await
        .expect("publish");
    let mut seen = vec![next(&mut orders).await, next(&mut orders).await]
        .into_iter()
        .map(|m| (m.topic, m.seq))
        .collect::<Vec<_>>();
    seen.sort();
    assert_eq!(
        seen,
        [("orders/asia".to_string(), 1), ("orders/us".to_string(), 2)]
    );

    client.unsubscribe("orders/#").await.expect("unsubscribe");
    client
        .publish_confirmed("orders/us", "third")
        .await
        .expect("publish");
    let after = tokio::time::timeout(Duration::from_millis(300), orders.next()).await;
    assert!(matches!(after, Ok(None)), "{after:?}");
    assert!(eventually(|| server.broker.stats().subscriptions == 0).await);
}

/// Checks a shared subscription from the client: two members split a
/// topic's messages with no overlap, and when one disconnects without
/// acknowledging, the other gets its messages too.
#[tokio::test]
async fn shared_subscriptions_split_messages_between_members() {
    let server = TestServer::memory(&Settings::default()).await;
    let publisher = Client::connect(server.config()).await.expect("connect");
    let first = Client::connect(server.config().ack_mode(AckMode::Manual))
        .await
        .expect("connect");
    let second = Client::connect(server.config().ack_mode(AckMode::Manual))
        .await
        .expect("connect");
    let mut first_jobs = first
        .subscribe_shared("jobs", "workers", StartFrom::New)
        .await
        .expect("join");
    let mut second_jobs = second
        .subscribe_shared("jobs", "workers", StartFrom::New)
        .await
        .expect("join");
    for n in 1..=6 {
        publisher
            .publish_confirmed("jobs", format!("job {n}"))
            .await
            .expect("publish");
    }
    let mut mine = Vec::new();
    let mut theirs = Vec::new();
    for _ in 0..3 {
        let message = next(&mut first_jobs).await;
        mine.push(message.seq);
        message.ack();
        theirs.push(next(&mut second_jobs).await.seq);
    }
    let mut all = [mine.clone(), theirs.clone()].concat();
    all.sort();
    assert_eq!(all, vec![1, 2, 3, 4, 5, 6]);

    second.close();
    let mut handed_over = Vec::new();
    for _ in 0..3 {
        let message = next(&mut first_jobs).await;
        handed_over.push(message.seq);
        message.ack();
    }
    handed_over.sort();
    theirs.sort();
    assert_eq!(handed_over, theirs);
    assert!(eventually(|| server.nothing_in_flight("jobs")).await);

    let refused = first
        .subscribe_shared("jobs/#", "workers", StartFrom::New)
        .await;
    assert!(
        matches!(
            &refused,
            Err(Error::Server {
                code: ErrorCode::BadRequest,
                ..
            })
        ),
        "{refused:?}"
    );
    let bad_name = first
        .subscribe_shared("jobs", "not a name", StartFrom::New)
        .await;
    assert!(
        matches!(
            &bad_name,
            Err(Error::Server {
                code: ErrorCode::BadRequest,
                ..
            })
        ),
        "{bad_name:?}"
    );
}

/// Checks that subscribing again to a pattern moves the topics it already
/// brought in to the new subscription, so dropping the old one loses nothing.
#[tokio::test]
async fn a_replaced_wildcard_subscription_keeps_its_topics() {
    let server = TestServer::memory(&Settings::default()).await;
    let client = Client::connect(server.config()).await.expect("connect");
    let mut old = client
        .subscribe("orders/#", StartFrom::New)
        .await
        .expect("subscribe");
    client
        .publish_confirmed("orders/eu", "first")
        .await
        .expect("publish");
    assert_eq!(next(&mut old).await.payload, "first");
    let mut new = client
        .subscribe("orders/#", StartFrom::New)
        .await
        .expect("subscribe again");
    drop(old);
    client
        .publish_confirmed("orders/eu", "second")
        .await
        .expect("publish");
    let second = next(&mut new).await;
    assert_eq!(
        (second.topic.as_str(), second.seq, second.payload.as_str()),
        ("orders/eu", 2, "second")
    );
}
