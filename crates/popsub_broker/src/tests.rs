//! Unit tests for the broker, using `tokio::sync::mpsc` channels as fake
//! client connections and `MemoryStorage` for topic logs and sessions.

use std::sync::Arc;
use std::time::Duration;

use popsub_config::Settings;
use popsub_config::settings::BrokerSettings;
use popsub_connection::{Client, Outbox, OutboxReceiver, Outgoing, QueueLimits};
use popsub_storage::{
    Durability, LogBounds, MemoryStorage, NewRecord, Record, Retention, StorageError,
    SubscriptionStore,
};
use serde_json::Value;
use tungstenite::protocol::Message as WsMessage;

use crate::replay::{ReplayPlan, plan};
use crate::topic::{CursorUpdate, DeliveryState};
use crate::{Broker, LogSettings, RetryOutcome, SharedBroker, session_key};
use popsub_protocol::StartFrom;

/// The receiving end of a fake client's outgoing queue.
type Inbox = OutboxReceiver;

/// Builds an empty broker with the default settings.
///
/// Returns:
///     The broker.
fn test_broker() -> Broker {
    Broker::new(Settings::default().broker)
}

/// Builds broker settings with a chosen window size.
///
/// Args:
///     max_inflight: The window size.
///
/// Returns:
///     The default broker settings with that window.
fn with_window(max_inflight: usize) -> BrokerSettings {
    BrokerSettings {
        max_inflight,
        ..Settings::default().broker
    }
}

/// Builds a shared broker on the given storage.
///
/// Args:
///     settings: The broker settings.
///     storage: Where logs and sessions live.
///     retention: The retention every topic gets.
///
/// Returns:
///     The shared broker.
fn shared_on(
    settings: BrokerSettings,
    storage: Arc<MemoryStorage>,
    retention: Retention,
) -> SharedBroker {
    SharedBroker::new(
        Broker::new(settings),
        storage,
        LogSettings {
            durability: Durability::None,
            retention,
            session_expiry: Duration::from_secs(86_400),
        },
    )
    .expect("memory storage supports durability none")
}

/// Builds a shared broker with default settings on a fresh `MemoryStorage`.
///
/// Args:
///     retention: The retention every topic gets.
///
/// Returns:
///     The shared broker.
fn shared(retention: Retention) -> SharedBroker {
    shared_on(
        Settings::default().broker,
        Arc::new(MemoryStorage::new()),
        retention,
    )
}

/// Registers a fake ephemeral client with a broker.
///
/// Args:
///     broker: The broker to register with.
///
/// Returns:
///     The client's id and its inbox.
fn connect(broker: &mut Broker) -> (String, Inbox) {
    connect_as(broker, None)
}

/// Registers a fake client, optionally holding a durable session.
///
/// Args:
///     broker: The broker to register with.
///     session: The session's storage key, if any.
///
/// Returns:
///     The client's id and its inbox.
fn connect_as(broker: &mut Broker, session: Option<&str>) -> (String, Inbox) {
    connect_with(broker, session, QueueLimits::default())
}

/// Registers a fake client with chosen outgoing queue limits.
///
/// Args:
///     broker: The broker to register with.
///     session: The session's storage key, if any.
///     limits: The client's queue limits.
///
/// Returns:
///     The client's id and its inbox.
fn connect_with(
    broker: &mut Broker,
    session: Option<&str>,
    limits: QueueLimits,
) -> (String, Inbox) {
    let (tx, rx) = Outbox::channel(limits);
    let mut client = Client::new(tx);
    client.session = session.map(str::to_string);
    let id = client.id.clone();
    broker.register_client(client);
    (id, rx)
}

/// Builds a record as the log would store it.
///
/// Args:
///     seq: The record's sequence number.
///     qos: The delivery level.
///
/// Returns:
///     A record on topic `t` with message id `m{seq}`.
fn record(seq: u64, qos: u8) -> Record {
    Record {
        topic: "t".to_string(),
        seq,
        message_id: format!("m{seq}"),
        publisher: "alice".to_string(),
        payload: format!("payload {seq}"),
        qos,
        published_at_ms: 1_000,
    }
}

/// Builds a record to publish.
///
/// Args:
///     message_id: The message id, or empty to have one generated.
///     qos: The delivery level.
///
/// Returns:
///     A record from `alice` whose payload is the message id.
fn new_record(message_id: &str, qos: u8) -> NewRecord {
    NewRecord {
        message_id: message_id.to_string(),
        publisher: "alice".to_string(),
        payload: message_id.to_string(),
        qos,
        published_at_ms: 0,
    }
}

/// Drains every frame already queued on a fake client's channel.
///
/// Args:
///     rx: The client's inbox.
///
/// Returns:
///     Each text frame parsed as JSON, in order.
fn frames(rx: &mut Inbox) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(Outgoing::Frame(frame)) = rx.try_recv() {
        if let WsMessage::Text(text) = frame {
            out.push(serde_json::from_str(&text).expect("frames are JSON"));
        }
    }
    out
}

/// Pulls the `seq` of every `message` frame.
///
/// Args:
///     frames: Parsed frames.
///
/// Returns:
///     The sequence numbers, in order.
fn seqs(frames: &[Value]) -> Vec<u64> {
    frames
        .iter()
        .filter(|f| f["type"] == "message")
        .map(|f| f["seq"].as_u64().expect("seq is a number"))
        .collect()
}

/// Waits up to two seconds for a client to have received `count` messages,
/// for checks on work the topic writer does in the background.
///
/// Args:
///     rx: The client's inbox.
///     count: How many `message` frames to wait for.
///
/// Returns:
///     Every frame received while waiting.
async fn wait_for_messages(rx: &mut Inbox, count: usize) -> Vec<Value> {
    let mut received = Vec::new();
    for _ in 0..200 {
        received.extend(frames(rx));
        if seqs(&received).len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    received
}

/// Publishes records `m1` to `mN` to topic `t`.
///
/// Args:
///     broker: The shared broker.
///     count: How many to publish.
///     qos: Their delivery level.
async fn publish_many(broker: &SharedBroker, count: u64, qos: u8) {
    for n in 1..=count {
        broker
            .publish("t", new_record(&format!("m{n}"), qos))
            .await
            .expect("publish");
    }
}

/// Checks that a new topic has the given name and no subscribers.
#[test]
fn test_topic_new() {
    let topic = super::topic::Topic::new("test_topic");
    assert_eq!(topic.name, "test_topic");
    assert!(topic.subscribers.is_empty());
    assert_eq!(topic.last_seq, 0);
}

/// Checks that a subscription's cursor sits below its oldest in-flight
/// record, and only moves for durable sessions.
#[test]
fn cursor_tracks_the_oldest_unacknowledged_record() {
    let mut state = DeliveryState::new(5, Some("s".to_string()));
    assert_eq!(state.cursor(), 4);
    assert_eq!(state.take_update("t"), None);
    state.next_seq = 9;
    state.inflight.insert(
        7,
        crate::topic::InFlight {
            record: record(7, 1),
            sent_at: 0,
            retries: 0,
        },
    );
    assert_eq!(state.cursor(), 6);
    assert_eq!(
        state.take_update("t"),
        Some(CursorUpdate {
            session: "s".to_string(),
            topic: "t".to_string(),
            acked_seq: 6
        })
    );
    assert_eq!(state.take_update("t"), None);

    let mut ephemeral = DeliveryState::new(1, None);
    ephemeral.next_seq = 50;
    assert_eq!(ephemeral.take_update("t"), None);
}

/// Checks client registration, removal and session lookups.
#[test]
fn clients_and_sessions_are_tracked() {
    let mut broker = test_broker();
    let (plain, _a) = connect(&mut broker);
    let (durable, _b) = connect_as(&mut broker, Some("key"));
    assert_eq!(broker.client_session(&plain), None);
    assert_eq!(broker.client_session(&durable).as_deref(), Some("key"));
    assert_eq!(broker.session_holder("key"), Some(durable.clone()));
    assert_eq!(broker.connected_sessions(), vec!["key".to_string()]);
    broker.remove_client(&plain);
    assert!(broker.client_sender(&plain).is_none());
    assert_eq!(broker.cleanup_client(&durable).as_deref(), Some("key"));
    assert!(broker.session_holder("key").is_none());
}

/// Checks that an unregistered client can't subscribe and that unsubscribing
/// reports the session whose stored subscription must go too.
#[test]
fn add_subscriber_requires_a_registered_client() {
    let mut broker = test_broker();
    let (id, _rx) = connect_as(&mut broker, Some("key"));
    assert!(broker.add_subscriber("t", id.clone(), 1, 0));
    assert!(broker.topics["t"].subscribers.contains_key(&id));
    assert!(!broker.add_subscriber("t", "ghost".to_string(), 1, 0));
    assert_eq!(broker.unsubscribe("t", &id).as_deref(), Some("key"));
    assert_eq!(broker.unsubscribe("t", &id), None);
}

/// Checks that a delivered record arrives as a tagged `message` frame with
/// its seq and without the publisher's username, and that QoS 0 leaves
/// nothing in flight.
#[test]
fn deliver_sends_a_tagged_frame_with_seq() {
    let mut broker = test_broker();
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id.clone(), 7, 6);
    broker.deliver(&record(7, 0), 1_000);
    let received = frames(&mut rx);
    assert_eq!(received.len(), 1);
    let frame = &received[0];
    assert_eq!(frame["type"], "message");
    assert_eq!(frame["topic"], "t");
    assert_eq!(frame["seq"], 7);
    assert_eq!(frame["message_id"], "m7");
    assert_eq!(frame["payload"], "payload 7");
    assert_eq!(frame["qos"], 0);
    assert_eq!(frame["published_at_ms"], 1_000);
    assert!(frame.get("publisher").is_none());
    assert!(broker.topics["t"].subscribers[&id].inflight.is_empty());
}

/// Checks that one subscriber's ACK clears the record only for itself, and
/// that ACKs from a client the record wasn't sent to are ignored.
#[test]
fn acks_are_per_subscriber() {
    let mut broker = test_broker();
    let (a, mut rx_a) = connect(&mut broker);
    let (b, mut rx_b) = connect(&mut broker);
    let (outsider, _rx_c) = connect(&mut broker);
    broker.add_subscriber("t", a.clone(), 1, 0);
    broker.add_subscriber("t", b.clone(), 1, 0);
    broker.deliver(&record(1, 1), 0);
    assert_eq!(seqs(&frames(&mut rx_a)), vec![1]);
    assert_eq!(seqs(&frames(&mut rx_b)), vec![1]);

    assert!(!broker.ack(&outsider, "t", 1, 0).accepted);
    assert!(broker.ack(&a, "t", 1, 0).accepted);
    assert!(!broker.ack(&a, "t", 1, 0).accepted);
    assert!(!broker.ack(&a, "t", 2, 0).accepted);
    assert!(!broker.ack(&a, "other", 1, 0).accepted);
    assert!(broker.topics["t"].subscribers[&a].inflight.is_empty());
    assert!(broker.topics["t"].subscribers[&b].inflight.contains_key(&1));
}

/// Checks that a retry goes only to the subscriber that hasn't acknowledged.
#[test]
fn retries_go_only_to_the_subscriber_that_has_not_acked() {
    let step = Settings::default().broker.ack_timeout_ms as i64 + 1;
    let mut broker = test_broker();
    let (a, mut rx_a) = connect(&mut broker);
    let (b, mut rx_b) = connect(&mut broker);
    broker.add_subscriber("t", a.clone(), 1, 0);
    broker.add_subscriber("t", b.clone(), 1, 0);
    broker.deliver(&record(1, 1), 0);
    frames(&mut rx_a);
    frames(&mut rx_b);
    broker.ack(&a, "t", 1, 0);

    let outcome = broker.retry_overdue(step);
    assert_eq!(
        outcome,
        RetryOutcome {
            resent: 1,
            stalled: 0
        }
    );
    assert!(frames(&mut rx_a).is_empty());
    assert_eq!(seqs(&frames(&mut rx_b)), vec![1]);
}

/// Checks that a record unacknowledged after `max_retries` resends stalls
/// its subscription with a `stalled` frame, pauses delivery, and that an ACK
/// lifts the stall and asks for a catch-up.
#[test]
fn running_out_of_retries_stalls_the_subscription() {
    let settings = Settings::default().broker;
    let step = settings.ack_timeout_ms as i64 + 1;
    let mut broker = test_broker();
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id.clone(), 1, 0);
    broker.deliver(&record(1, 1), 0);
    frames(&mut rx);

    assert_eq!(broker.retry_overdue(0).resent, 0);
    for attempt in 1..=settings.max_retries {
        assert_eq!(broker.retry_overdue(step * attempt as i64).resent, 1);
        assert_eq!(seqs(&frames(&mut rx)), vec![1]);
    }
    let last = step * (settings.max_retries as i64 + 1);
    assert_eq!(
        broker.retry_overdue(last),
        RetryOutcome {
            resent: 0,
            stalled: 1
        }
    );
    let stalled = frames(&mut rx);
    assert_eq!(stalled.len(), 1);
    assert_eq!(stalled[0]["type"], "stalled");
    assert_eq!(stalled[0]["seq"], 1);

    broker.deliver(&record(2, 0), last);
    assert!(frames(&mut rx).is_empty());
    assert_eq!(broker.retry_overdue(last * 2).resent, 0);

    let outcome = broker.ack(&id, "t", 1, last);
    assert!(outcome.accepted);
    assert!(outcome.catch_up);
    assert!(broker.topics["t"].subscribers[&id].stalled.is_none());
}

/// Checks that a full window holds back later records, QoS 0 included, and
/// that an ACK asks for a catch-up.
#[test]
fn a_full_window_holds_back_later_records() {
    let mut broker = Broker::new(with_window(2));
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id.clone(), 1, 0);
    broker.deliver(&record(1, 1), 0);
    broker.deliver(&record(2, 1), 0);
    broker.deliver(&record(3, 0), 0);
    assert_eq!(seqs(&frames(&mut rx)), vec![1, 2]);
    assert_eq!(broker.wants("t", &id), None);
    let outcome = broker.ack(&id, "t", 1, 0);
    assert!(outcome.catch_up);
    assert_eq!(broker.wants("t", &id), Some(3));
}

/// Checks that a session's cursor only advances over acknowledged records
/// with no gap, whatever order the ACKs arrive in.
#[test]
fn cursors_advance_only_over_contiguous_acks() {
    let mut broker = test_broker();
    let (id, mut rx) = connect_as(&mut broker, Some("key"));
    broker.add_subscriber("t", id.clone(), 1, 0);
    for seq in 1..=3 {
        broker.deliver(&record(seq, 1), 0);
    }
    frames(&mut rx);
    assert_eq!(broker.ack(&id, "t", 2, 0).cursor, None);
    assert_eq!(
        broker.ack(&id, "t", 1, 0).cursor.map(|c| c.acked_seq),
        Some(2)
    );
    assert_eq!(
        broker.ack(&id, "t", 3, 0).cursor.map(|c| c.acked_seq),
        Some(3)
    );
}

/// Checks that QoS 0 records move a session's cursor as soon as they are sent.
#[test]
fn qos0_records_move_the_cursor_when_sent() {
    let mut broker = test_broker();
    let (id, _rx) = connect_as(&mut broker, Some("key"));
    broker.add_subscriber("t", id, 1, 0);
    let updates = broker.deliver(&record(1, 0), 0).cursors;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].acked_seq, 1);
}

/// Checks that `SharedBroker` keeps working after a task panics while holding its lock.
#[test]
fn shared_broker_recovers_from_a_poisoned_lock() {
    let shared = shared(Retention::default());
    let poisoner = shared.clone();
    let result = std::thread::spawn(move || {
        poisoner.with(|_| panic!("simulated panic while holding the broker lock"))
    })
    .join();
    assert!(result.is_err());
    let (id, _rx) = shared.with(connect);
    assert!(shared.with(|broker| broker.add_subscriber("after_panic", id, 1, 0)));
}

/// Checks that a durability stronger than the backend offers is refused at construction.
#[test]
fn unsupported_durability_is_refused() {
    let result = SharedBroker::new(
        test_broker(),
        Arc::new(MemoryStorage::new()),
        LogSettings {
            durability: Durability::Synced,
            retention: Retention::default(),
            session_expiry: Duration::from_secs(86_400),
        },
    );
    assert!(matches!(result, Err(StorageError::Unsupported(_))));
}

/// Checks that session keys can't collide across users, whatever the ids contain.
#[test]
fn session_keys_are_per_user() {
    assert_ne!(session_key("alice", "s"), session_key("bob", "s"));
    assert_ne!(session_key("a:b", "c"), session_key("a", "b:c"));
    assert_eq!(session_key("alice", "s"), session_key("alice", "s"));
}

/// Checks the replay plan for each `from` option on a log holding 1 to 10.
#[test]
fn replay_plan_on_an_untrimmed_log() {
    let bounds = LogBounds {
        first_seq: 1,
        last_seq: 10,
    };
    let starts = |from, seq_at| plan(from, bounds, seq_at);
    assert_eq!(
        starts(StartFrom::New, None),
        ReplayPlan {
            start: 11,
            truncated: false
        }
    );
    assert_eq!(starts(StartFrom::Resume, None).start, 11);
    assert_eq!(starts(StartFrom::Last(3), None).start, 8);
    assert_eq!(starts(StartFrom::Last(0), None).start, 11);
    assert_eq!(
        starts(StartFrom::Last(50), None),
        ReplayPlan {
            start: 1,
            truncated: false
        }
    );
    assert_eq!(starts(StartFrom::Seq(0), None).start, 1);
    assert_eq!(starts(StartFrom::Seq(4), None).start, 4);
    assert_eq!(starts(StartFrom::Seq(99), None).start, 99);
    assert_eq!(
        starts(StartFrom::SinceMs(5), Some(6)),
        ReplayPlan {
            start: 6,
            truncated: false
        }
    );
    assert!(!starts(StartFrom::SinceMs(5), Some(1)).truncated);
    assert_eq!(starts(StartFrom::SinceMs(5), None).start, 11);
}

/// Checks that asking for trimmed history starts at the oldest retained
/// record and reports a truncation, while asking for retained history doesn't.
#[test]
fn replay_plan_on_a_trimmed_log() {
    let bounds = LogBounds {
        first_seq: 6,
        last_seq: 10,
    };
    let truncated = |start| ReplayPlan {
        start,
        truncated: true,
    };
    let clean = |start| ReplayPlan {
        start,
        truncated: false,
    };
    assert_eq!(plan(StartFrom::Seq(2), bounds, None), truncated(6));
    assert_eq!(plan(StartFrom::Seq(6), bounds, None), clean(6));
    assert_eq!(plan(StartFrom::Last(8), bounds, None), truncated(6));
    assert_eq!(plan(StartFrom::Last(5), bounds, None), clean(6));
    assert!(!plan(StartFrom::New, bounds, None).truncated);
    assert_eq!(plan(StartFrom::SinceMs(0), bounds, Some(6)), truncated(6));
    assert_eq!(plan(StartFrom::SinceMs(0), bounds, Some(8)), clean(8));
    let emptied = LogBounds {
        first_seq: 11,
        last_seq: 10,
    };
    assert_eq!(plan(StartFrom::SinceMs(0), emptied, None), truncated(11));
}

/// Checks that a subscriber is told its replay range before anything else,
/// gets no history by default, and then receives live records in order.
#[tokio::test]
async fn subscribe_announces_the_replay_range_then_delivers_live() {
    let broker = shared(Retention::default());
    publish_many(&broker, 3, 0).await;
    let (id, mut rx) = broker.with(connect);
    let outcome = broker
        .subscribe("t", &id, StartFrom::default(), None)
        .await
        .expect("subscribe");
    assert!(outcome.added);
    assert_eq!((outcome.replay_from, outcome.replay_to), (4, 3));
    let received = frames(&mut rx);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["type"], "subscribed");
    assert_eq!(received[0]["replay_from"], 4);
    assert_eq!(received[0]["replay_to"], 3);

    broker
        .publish("t", new_record("m4", 0))
        .await
        .expect("publish");
    assert_eq!(seqs(&frames(&mut rx)), vec![4]);
}

/// Checks that `last` replays the newest records with their original seq and
/// live delivery carries on after them without a repeat.
#[tokio::test]
async fn last_replays_original_seqs_then_live() {
    let broker = shared(Retention::default());
    publish_many(&broker, 5, 0).await;
    let (id, mut rx) = broker.with(connect);
    let outcome = broker
        .subscribe("t", &id, StartFrom::Last(2), None)
        .await
        .expect("subscribe");
    assert_eq!(outcome.replayed, 2);
    broker
        .publish("t", new_record("m6", 0))
        .await
        .expect("publish");
    let received = frames(&mut rx);
    assert_eq!(seqs(&received), vec![4, 5, 6]);
    assert_eq!(received[1]["message_id"], "m4");
}

/// Checks that `since_ms` replays from the first record at or after the time.
#[tokio::test]
async fn since_ms_replays_from_a_time() {
    let broker = shared(Retention::default());
    publish_many(&broker, 2, 0).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let cutoff = chrono::Utc::now().timestamp_millis();
    tokio::time::sleep(Duration::from_millis(5)).await;
    broker
        .publish("t", new_record("m3", 0))
        .await
        .expect("publish");
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::SinceMs(cutoff), None)
        .await
        .expect("subscribe");
    assert_eq!(seqs(&frames(&mut rx)), vec![3]);
}

/// Checks that asking for trimmed history sends a `truncated` frame before
/// the oldest retained record.
#[tokio::test]
async fn trimmed_history_is_reported_before_replay() {
    let broker = shared(Retention {
        max_messages: Some(3),
        ..Retention::default()
    });
    publish_many(&broker, 6, 0).await;
    let (id, mut rx) = broker.with(connect);
    let outcome = broker
        .subscribe("t", &id, StartFrom::Seq(1), None)
        .await
        .expect("subscribe");
    assert!(outcome.truncated);
    let received = frames(&mut rx);
    let types: Vec<&str> = received
        .iter()
        .map(|f| f["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec!["subscribed", "truncated", "message", "message", "message"]
    );
    assert_eq!(received[1]["first_available"], 4);
    assert_eq!(seqs(&received), vec![4, 5, 6]);
}

/// Checks that a publisher retrying the same message id gets the original
/// seq and id back and subscribers receive it once.
#[tokio::test]
async fn a_retried_publish_is_stored_and_delivered_once() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    let first = broker
        .publish("t", new_record("same", 0))
        .await
        .expect("publish");
    let again = broker
        .publish("t", new_record("same", 0))
        .await
        .expect("publish");
    assert!(!first.duplicate);
    assert!(again.duplicate);
    assert_eq!((first.seq, first.message_id.as_str()), (1, "same"));
    assert_eq!(again.seq, first.seq);
    assert_eq!(seqs(&frames(&mut rx)), vec![1]);
}

/// Checks that a missing message id is replaced with a different UUID each
/// time and returned to the publisher.
#[tokio::test]
async fn missing_message_ids_are_generated() {
    let broker = shared(Retention::default());
    let a = broker
        .publish("t", new_record("", 0))
        .await
        .expect("publish");
    let b = broker
        .publish("t", new_record("", 0))
        .await
        .expect("publish");
    assert_eq!(a.message_id.len(), 36);
    assert_ne!(a.message_id, b.message_id);
    assert_eq!((a.seq, b.seq), (1, 2));
}

/// Checks that subscribing a client that isn't registered adds nothing.
#[tokio::test]
async fn subscribing_an_unknown_client_adds_nothing() {
    let broker = shared(Retention::default());
    publish_many(&broker, 2, 0).await;
    let outcome = broker
        .subscribe("t", "ghost", StartFrom::Seq(1), None)
        .await
        .expect("subscribe");
    assert!(!outcome.added);
    assert!(broker.with(|b| b.topics.get("t").is_none_or(|t| t.subscribers.is_empty())));
}

/// Checks that publishing trims each topic to its retention and that
/// `trim_all` applies the age limit to quiet topics.
#[tokio::test]
async fn retention_is_applied_on_publish_and_by_trim_all() {
    let broker = shared(Retention {
        max_messages: Some(2),
        max_age_ms: Some(60_000),
        max_bytes: None,
    });
    publish_many(&broker, 5, 0).await;
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::Last(10), None)
        .await
        .expect("subscribe");
    assert_eq!(seqs(&frames(&mut rx)), vec![4, 5]);

    let much_later = chrono::Utc::now().timestamp_millis() + 120_000;
    assert_eq!(broker.trim_all(much_later).await, 2);
    let (other, mut other_rx) = broker.with(connect);
    broker
        .subscribe("t", &other, StartFrom::Seq(1), None)
        .await
        .expect("subscribe");
    let received = frames(&mut other_rx);
    assert_eq!(received.len(), 2);
    assert_eq!(received[1]["type"], "truncated");
    assert_eq!(received[1]["first_available"], 6);
}

/// Checks that concurrent publishers to one topic produce one gap-free,
/// ordered stream for a subscriber.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publishers_are_delivered_in_seq_order() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    let mut tasks = Vec::new();
    for writer in 0..8 {
        let broker = broker.clone();
        tasks.push(tokio::spawn(async move {
            for n in 0..25 {
                broker
                    .publish("t", new_record(&format!("w{writer}-{n}"), 0))
                    .await
                    .expect("publish");
            }
        }));
    }
    for task in tasks {
        task.await.expect("publisher task");
    }
    assert_eq!(seqs(&frames(&mut rx)), (1..=200).collect::<Vec<_>>());
}

/// Checks that a subscriber joining with full replay while records are being
/// published receives every record exactly once, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_and_live_delivery_never_overlap_or_gap() {
    let broker = shared(Retention::default());
    publish_many(&broker, 50, 0).await;
    let publisher = {
        let broker = broker.clone();
        tokio::spawn(async move {
            for n in 51..=300 {
                broker
                    .publish("t", new_record(&format!("m{n}"), 0))
                    .await
                    .expect("publish");
            }
        })
    };
    tokio::task::yield_now().await;
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::Seq(1), None)
        .await
        .expect("subscribe");
    publisher.await.expect("publisher task");
    assert_eq!(seqs(&frames(&mut rx)), (1..=300).collect::<Vec<_>>());
}

/// Checks that a subscriber whose window is full gets the records it missed
/// from the log once it acknowledges, in order and without gaps.
#[tokio::test]
async fn acks_catch_a_full_window_up_from_the_log() {
    let broker = shared_on(
        with_window(2),
        Arc::new(MemoryStorage::new()),
        Retention::default(),
    );
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    publish_many(&broker, 5, 1).await;
    let mut received = seqs(&frames(&mut rx));
    assert_eq!(received, vec![1, 2]);
    for seq in 1..=5 {
        assert!(broker.ack(&id, "t", seq).await);
        if seq < 4 {
            let more = wait_for_messages(&mut rx, 1).await;
            received.extend(seqs(&more));
        }
    }
    assert_eq!(received, vec![1, 2, 3, 4, 5]);
}

/// Checks that a durable session gets back what it hadn't acknowledged, and
/// what was published while it was away, after reconnecting.
#[tokio::test]
async fn a_session_resumes_from_its_cursor_after_reconnecting() {
    let broker = shared(Retention::default());
    let key = session_key("alice", "phone");
    let (first, mut rx) = broker.with(|b| connect_as(b, None));
    assert!(
        broker
            .attach_session(&first, &key, false)
            .await
            .expect("attach")
            .is_empty()
    );
    broker
        .subscribe("t", &first, StartFrom::default(), None)
        .await
        .expect("subscribe");
    publish_many(&broker, 2, 1).await;
    assert_eq!(seqs(&frames(&mut rx)), vec![1, 2]);
    assert!(broker.ack(&first, "t", 1).await);
    broker.cleanup_client(&first);
    broker
        .publish("t", new_record("m3", 1))
        .await
        .expect("publish");

    let (second, mut rx) = broker.with(connect);
    let stored = broker
        .attach_session(&second, &key, false)
        .await
        .expect("attach");
    assert_eq!(stored.len(), 1);
    assert_eq!((stored[0].topic.as_str(), stored[0].acked_seq), ("t", 1));
    broker
        .subscribe("t", &second, StartFrom::Resume, None)
        .await
        .expect("subscribe");
    assert_eq!(seqs(&frames(&mut rx)), vec![2, 3]);
}

/// Checks that a session resumes from its stored cursor when the broker
/// itself is rebuilt on the same storage, as after a restart.
#[tokio::test]
async fn a_session_resumes_after_the_broker_restarts() {
    let storage = Arc::new(MemoryStorage::new());
    let key = session_key("alice", "phone");
    {
        let broker = shared_on(
            Settings::default().broker,
            storage.clone(),
            Retention::default(),
        );
        let (id, mut rx) = broker.with(connect);
        broker
            .attach_session(&id, &key, false)
            .await
            .expect("attach");
        broker
            .subscribe("t", &id, StartFrom::default(), None)
            .await
            .expect("subscribe");
        publish_many(&broker, 3, 1).await;
        frames(&mut rx);
        assert!(broker.ack(&id, "t", 1).await);
    }
    let broker = shared_on(Settings::default().broker, storage, Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .attach_session(&id, &key, false)
        .await
        .expect("attach");
    broker
        .subscribe("t", &id, StartFrom::Resume, None)
        .await
        .expect("subscribe");
    assert_eq!(seqs(&frames(&mut rx)), vec![2, 3]);
}

/// Checks that attaching a session held by another connection closes that
/// connection and moves the session to the new one.
#[tokio::test]
async fn a_new_connection_takes_over_a_session() {
    let broker = shared(Retention::default());
    let key = session_key("alice", "phone");
    let (old, mut old_rx) = broker.with(connect);
    broker
        .attach_session(&old, &key, false)
        .await
        .expect("attach");
    let (new, _new_rx) = broker.with(connect);
    broker
        .attach_session(&new, &key, false)
        .await
        .expect("attach");
    assert_eq!(
        old_rx.try_recv(),
        Some(Outgoing::Close(
            popsub_protocol::CloseCode::SessionTakenOver
        ))
    );
    assert!(broker.with(|b| !b.clients.contains_key(&old)));
    assert_eq!(broker.with(|b| b.session_holder(&key)), Some(new));
}

/// Checks that `clean_start` discards a session's stored subscriptions.
#[tokio::test]
async fn clean_start_discards_stored_subscriptions() {
    let broker = shared(Retention::default());
    let key = session_key("alice", "phone");
    let (id, _rx) = broker.with(connect);
    broker
        .attach_session(&id, &key, false)
        .await
        .expect("attach");
    broker
        .subscribe("t", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    broker.cleanup_client(&id);
    let (again, _rx) = broker.with(connect);
    assert!(
        broker
            .attach_session(&again, &key, true)
            .await
            .expect("attach")
            .is_empty()
    );
    let (third, _rx) = broker.with(connect);
    assert!(
        broker
            .attach_session(&third, &key, false)
            .await
            .expect("attach")
            .is_empty()
    );
}

/// Checks that unsubscribing a session removes its stored subscription too.
#[tokio::test]
async fn unsubscribing_forgets_the_stored_subscription() {
    let storage = Arc::new(MemoryStorage::new());
    let broker = shared_on(
        Settings::default().broker,
        storage.clone(),
        Retention::default(),
    );
    let key = session_key("alice", "phone");
    let (id, _rx) = broker.with(connect);
    broker
        .attach_session(&id, &key, false)
        .await
        .expect("attach");
    broker
        .subscribe("t", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    assert_eq!(storage.load(&key).await.expect("load").len(), 1);
    broker.unsubscribe("t", &id).await.expect("unsubscribe");
    assert!(storage.load(&key).await.expect("load").is_empty());
}

/// Checks that expiry keeps connected sessions and removes idle ones.
#[tokio::test]
async fn idle_sessions_expire_and_connected_ones_do_not() {
    let storage = Arc::new(MemoryStorage::new());
    let broker = shared_on(
        Settings::default().broker,
        storage.clone(),
        Retention::default(),
    );
    let (connected, _rx) = broker.with(connect);
    broker
        .attach_session(&connected, "connected", false)
        .await
        .expect("attach");
    storage.subscribe("idle", "t", 0).await.expect("subscribe");
    storage.touch("idle", 1).await.expect("touch");
    let now = chrono::Utc::now().timestamp_millis() + 10_000;
    assert_eq!(broker.expire_sessions(now, Duration::from_secs(5)).await, 1);
    assert!(storage.load("idle").await.expect("load").is_empty());
}

/// Checks that topics have independent sequences and subscribers.
#[tokio::test]
async fn topics_are_independent() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("a", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    broker
        .publish("b", new_record("x", 0))
        .await
        .expect("publish");
    broker
        .publish("a", new_record("y", 0))
        .await
        .expect("publish");
    let received = frames(&mut rx);
    assert_eq!(seqs(&received), vec![1]);
    assert_eq!(received[1]["topic"], "a");
}

/// Checks that the `subscribed` frame echoes the subscribe request's id and
/// that the advertised limits match the configuration.
#[tokio::test]
async fn subscribed_echoes_the_request_id_and_limits_match_settings() {
    let broker = shared_on(
        with_window(7),
        Arc::new(MemoryStorage::new()),
        Retention::default(),
    );
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe(
            "t",
            &id,
            StartFrom::New,
            Some(popsub_protocol::RequestId::Number(9)),
        )
        .await
        .expect("subscribe");
    let received = frames(&mut rx);
    assert_eq!(received[0]["type"], "subscribed");
    assert_eq!(received[0]["id"], 9);
    assert_eq!(broker.max_inflight(), 7);
    assert_eq!(broker.session_expiry(), Duration::from_secs(86_400));
}

/// Checks that `close_all` sends every connected client a going-away close frame.
#[test]
fn close_all_sends_every_client_a_close_frame() {
    let broker = shared(Retention::default());
    let (_a, mut rx_a) = broker.with(connect);
    let (_b, mut rx_b) = broker.with(connect);
    assert_eq!(broker.close_all(popsub_protocol::CloseCode::GoingAway), 2);
    for rx in [&mut rx_a, &mut rx_b] {
        assert_eq!(
            rx.try_recv(),
            Some(Outgoing::Close(popsub_protocol::CloseCode::GoingAway))
        );
    }
    assert_eq!(broker.close_all(popsub_protocol::CloseCode::GoingAway), 0);
}

/// Limits small enough that one delivery fills the queue past its soft limit.
const TINY_QUEUE: QueueLimits = QueueLimits {
    soft_bytes: 100,
    hard_bytes: 1_000_000,
};

/// Checks that a subscriber whose queue is over its soft limit is skipped
/// and reported once, rather than having frames piled up.
#[test]
fn a_busy_subscriber_is_skipped_and_reported_once() {
    let mut broker = test_broker();
    let (id, mut rx) = connect_with(&mut broker, None, TINY_QUEUE);
    broker.add_subscriber("t", id.clone(), 1, 0);
    assert!(broker.deliver(&record(1, 0), 0).backed_up.is_empty());
    let second = broker.deliver(&record(2, 0), 0);
    assert_eq!(second.backed_up, vec![id.clone()]);
    assert!(broker.deliver(&record(3, 0), 0).backed_up.is_empty());
    assert_eq!(seqs(&frames(&mut rx)), vec![1]);
    assert_eq!(broker.wants("t", &id), Some(2));
}

/// Checks that live records skipped while a subscriber's queue was full
/// reach it, in order and without gaps, once it drains.
#[tokio::test]
async fn a_busy_subscriber_catches_up_when_its_queue_drains() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(|b| connect_with(b, None, TINY_QUEUE));
    broker
        .subscribe("t", &id, StartFrom::New, None)
        .await
        .expect("subscribe");
    frames(&mut rx);
    publish_many(&broker, 20, 0).await;
    let mut received = Vec::new();
    while received.len() < 20 {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(Outgoing::Frame(WsMessage::Text(text)))) => {
                let value: Value = serde_json::from_str(&text).expect("json");
                received.extend(seqs(&[value]));
            }
            other => panic!("expected frames, got {other:?}"),
        }
    }
    assert_eq!(received, (1..=20).collect::<Vec<_>>());
}

/// Checks that a large replay is paced by the subscriber's queue: it never
/// holds much more than the soft limit, and every record still arrives.
#[tokio::test]
async fn replay_is_paced_by_the_subscribers_queue() {
    let broker = shared(Retention::default());
    publish_many(&broker, 200, 0).await;
    let (id, mut rx) = broker.with(|b| connect_with(b, None, TINY_QUEUE));
    let outbox = broker.with(|b| b.client_sender(&id)).expect("registered");
    broker
        .subscribe("t", &id, StartFrom::Seq(1), None)
        .await
        .expect("subscribe");
    let mut received = Vec::new();
    let mut peak = 0;
    while received.len() < 200 {
        peak = peak.max(outbox.queued_bytes());
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(Outgoing::Frame(WsMessage::Text(text)))) => {
                let value: Value = serde_json::from_str(&text).expect("json");
                received.extend(seqs(&[value]));
            }
            other => panic!("expected frames, got {other:?}"),
        }
    }
    assert_eq!(received, (1..=200).collect::<Vec<_>>());
    assert!(peak < 1_000, "the queue peaked at {peak} bytes");
}

/// Checks that a user's live subscriptions are listed across connections,
/// and that closing a user closes only that user's connections.
#[test]
fn a_users_connections_can_be_listed_and_closed() {
    let broker = shared(Retention::default());
    let (alice_one, mut rx_one) = broker.with(connect);
    let (alice_two, mut rx_two) = broker.with(connect);
    let (bob, mut rx_bob) = broker.with(connect);
    broker.with(|b| {
        for (id, name) in [(&alice_one, "alice"), (&alice_two, "alice"), (&bob, "bob")] {
            b.clients.get_mut(id).expect("client").username = Some(name.to_string());
        }
        b.add_subscriber("news", alice_one.clone(), 1, 0);
        b.add_subscriber("orders", alice_two.clone(), 1, 0);
        b.add_subscriber("news", bob.clone(), 1, 0);
    });
    let mut subscriptions = broker.with(|b| b.subscriptions_of_user("alice"));
    subscriptions.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        subscriptions,
        vec![
            (alice_one.clone(), "news".to_string()),
            (alice_two.clone(), "orders".to_string())
        ]
    );
    assert_eq!(
        broker.close_user("alice", popsub_protocol::CloseCode::AccountDisabled),
        2
    );
    for rx in [&mut rx_one, &mut rx_two] {
        assert_eq!(
            rx.try_recv(),
            Some(Outgoing::Close(popsub_protocol::CloseCode::AccountDisabled))
        );
    }
    assert_eq!(rx_bob.try_recv(), None);
}

/// Checks that failed appends mark storage as failing until one succeeds.
#[tokio::test]
async fn failed_appends_mark_storage_as_failing() {
    let storage = popsub_storage::FaultyStorage::new(Arc::new(MemoryStorage::new()));
    let broker = SharedBroker::new(
        Broker::new(BrokerSettings::default()),
        Arc::new(storage.clone()),
        LogSettings {
            durability: Durability::None,
            retention: Retention::default(),
            session_expiry: Duration::from_secs(60),
        },
    )
    .expect("broker");
    let window = Duration::from_secs(30);
    assert_eq!(broker.storage_failing(window), None);
    storage.set_failing(true);
    assert!(
        broker
            .publish("orders", new_record("lost", 1))
            .await
            .is_err()
    );
    assert!(broker.storage_failing(window).is_some());
    storage.set_failing(false);
    broker
        .publish("orders", new_record("kept", 1))
        .await
        .expect("publish");
    assert_eq!(broker.storage_failing(window), None);
}

/// Checks that `stats` counts connections, topics, subscriptions and the
/// QoS 1 records in flight, and that an ack takes one out.
#[tokio::test]
async fn stats_count_the_brokers_state() {
    let broker = shared(Retention::default());
    let (client_id, _rx) = broker.with(connect);
    broker
        .subscribe("orders", &client_id, StartFrom::New, None)
        .await
        .expect("subscribe");
    broker
        .publish("orders", new_record("one", 1))
        .await
        .expect("publish");
    let stats = broker.stats();
    assert_eq!(
        (
            stats.connections,
            stats.topics,
            stats.subscriptions,
            stats.in_flight
        ),
        (1, 1, 1, 1)
    );
    assert!(broker.ack(&client_id, "orders", 1).await);
    assert_eq!(broker.stats().in_flight, 0);
}
