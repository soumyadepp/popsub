//! Unit tests for the broker, using `tokio::sync::mpsc` channels as fake
//! client connections and `MemoryStorage` for topic logs.

use std::sync::Arc;

use popsub_client::Client;
use popsub_config::Settings;
use popsub_storage::{
    Durability, LogBounds, MemoryStorage, NewRecord, Record, Retention, StorageError,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tungstenite::protocol::Message as WsMessage;

use crate::frame::StartFrom;
use crate::replay::{ReplayPlan, plan};
use crate::{Broker, LogSettings, RetryOutcome, SharedBroker};

/// Builds an empty broker with the default settings.
///
/// Returns:
///     The broker.
fn test_broker() -> Broker {
    Broker::new(Settings::default().broker)
}

/// Builds a shared broker on a fresh `MemoryStorage`.
///
/// Args:
///     retention: The retention every topic gets.
///
/// Returns:
///     The shared broker.
fn shared(retention: Retention) -> SharedBroker {
    SharedBroker::new(
        test_broker(),
        Arc::new(MemoryStorage::new()),
        LogSettings {
            durability: Durability::None,
            retention,
        },
    )
    .expect("memory storage supports durability none")
}

/// Registers a fake client with a broker.
///
/// Args:
///     broker: The broker to register with.
///
/// Returns:
///     The client's id and the receiving end of its channel.
fn connect(broker: &mut Broker) -> (String, mpsc::UnboundedReceiver<WsMessage>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let client = Client::new(tx);
    let id = client.id.clone();
    broker.register_client(client);
    (id, rx)
}

/// Builds a record as the log would store it.
///
/// Args:
///     seq: The record's sequence number.
///     message_id: The message id.
///     qos: The delivery level.
///
/// Returns:
///     A record on topic `t`.
fn record(seq: u64, message_id: &str, qos: u8) -> Record {
    Record {
        topic: "t".to_string(),
        seq,
        message_id: message_id.to_string(),
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
///     payload: The message body.
///
/// Returns:
///     A QoS 0 record from `alice`.
fn new_record(message_id: &str, payload: &str) -> NewRecord {
    NewRecord {
        message_id: message_id.to_string(),
        publisher: "alice".to_string(),
        payload: payload.to_string(),
        qos: 0,
        published_at_ms: 0,
    }
}

/// Drains every frame already queued on a fake client's channel.
///
/// Args:
///     rx: The receiving end of the client's channel.
///
/// Returns:
///     Each frame parsed as JSON, in order.
fn frames(rx: &mut mpsc::UnboundedReceiver<WsMessage>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(WsMessage::Text(text)) = rx.try_recv() {
        out.push(serde_json::from_str(&text).expect("frames are JSON"));
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

/// Publishes payloads `m1` to `mN` to topic `t`.
///
/// Args:
///     broker: The shared broker.
///     count: How many to publish.
async fn publish_many(broker: &SharedBroker, count: u64) {
    for n in 1..=count {
        broker
            .publish("t", new_record(&format!("m{n}"), &format!("m{n}")))
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
}

/// Checks that subscribing and unsubscribing change the topic's subscriber set.
#[test]
fn test_topic_subscribe_and_unsubscribe() {
    let mut topic = super::topic::Topic::new("test_topic");
    topic.subscribe("client1".to_string());
    assert!(topic.subscribers.contains("client1"));
    topic.unsubscribe(&"client1".to_string());
    assert!(!topic.subscribers.contains("client1"));
}

/// Checks that a new broker starts with no topics, clients or pending records.
#[test]
fn test_broker_new() {
    let broker = test_broker();
    assert!(broker.topics.is_empty());
    assert!(broker.clients.is_empty());
    assert!(broker.pending_acks.is_empty());
}

/// Checks that a registered client is stored by id and removed by `remove_client`.
#[test]
fn test_broker_register_and_remove_client() {
    let mut broker = test_broker();
    let (id, _rx) = connect(&mut broker);
    assert!(broker.clients.contains_key(&id));
    assert!(broker.client_sender(&id).is_some());
    broker.remove_client(&id);
    assert!(!broker.clients.contains_key(&id));
    assert!(broker.client_sender(&id).is_none());
}

/// Checks that adding a subscriber creates the topic and that an unregistered
/// client is not added.
#[test]
fn add_subscriber_requires_a_registered_client() {
    let mut broker = test_broker();
    let (id, _rx) = connect(&mut broker);
    assert!(broker.add_subscriber("t", id.clone()));
    assert!(broker.topics["t"].subscribers.contains(&id));
    assert!(!broker.add_subscriber("t", "ghost".to_string()));
    assert!(!broker.topics["t"].subscribers.contains("ghost"));
    broker.unsubscribe("t", &id);
    assert!(!broker.topics["t"].subscribers.contains(&id));
}

/// Checks that a delivered record arrives as a tagged `message` frame with
/// its seq and without the publisher's username.
#[test]
fn deliver_sends_a_tagged_frame_with_seq() {
    let mut broker = test_broker();
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id);
    assert_eq!(broker.deliver(&record(7, "m7", 0), 1_000), 1);
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
    assert!(broker.pending_acks.is_empty());
}

/// Checks that a QoS 1 record is held in `pending_acks` until acknowledged.
#[test]
fn qos1_delivery_is_pending_until_acked() {
    let mut broker = test_broker();
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id);
    broker.deliver(&record(1, "q1", 1), 1_000);
    assert!(broker.pending_acks.contains_key("q1"));
    assert_eq!(seqs(&frames(&mut rx)), vec![1]);
    broker.handle_ack("q1");
    assert!(!broker.pending_acks.contains_key("q1"));
    broker.handle_ack("unknown");
    assert!(broker.pending_acks.is_empty());
}

/// Checks that delivering to a topic without subscribers, or to a closed
/// channel, doesn't panic and reaches nobody.
#[test]
fn deliver_without_live_subscribers_reaches_nobody() {
    let mut broker = test_broker();
    assert_eq!(broker.deliver(&record(1, "a", 0), 0), 0);
    let (id, rx) = connect(&mut broker);
    broker.add_subscriber("t", id);
    drop(rx);
    assert_eq!(broker.deliver(&record(2, "b", 0), 0), 0);
}

/// Checks that `cleanup_client` removes the client and its subscriptions.
#[test]
fn test_broker_cleanup_client() {
    let mut broker = test_broker();
    let (id, _rx) = connect(&mut broker);
    broker.add_subscriber("t", id.clone());
    broker.cleanup_client(&id);
    assert!(!broker.clients.contains_key(&id));
    assert!(!broker.topics["t"].subscribers.contains(&id));
}

/// Checks that an unacknowledged QoS 1 record is resent `max_retries` times
/// with its original seq, then dropped, and that a late ACK is ignored.
#[test]
fn test_broker_qos1_retry_mechanism() {
    let settings = Settings::default().broker;
    let step = settings.ack_timeout_ms as i64 + 1;
    let mut broker = test_broker();
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id);
    let sent_at = 10_000;
    broker.deliver(&record(3, "retry", 1), sent_at);
    assert_eq!(seqs(&frames(&mut rx)), vec![3]);

    assert_eq!(broker.retry_overdue(sent_at).resent, 0);
    for attempt in 1..=settings.max_retries {
        let outcome = broker.retry_overdue(sent_at + step * attempt as i64);
        assert_eq!(
            outcome,
            RetryOutcome {
                resent: 1,
                dropped: 0
            }
        );
        assert_eq!(seqs(&frames(&mut rx)), vec![3]);
        assert_eq!(broker.pending_acks["retry"].retries, attempt);
    }

    let last = sent_at + step * (settings.max_retries as i64 + 1);
    assert_eq!(
        broker.retry_overdue(last),
        RetryOutcome {
            resent: 0,
            dropped: 1
        }
    );
    assert!(frames(&mut rx).is_empty());
    assert!(!broker.pending_acks.contains_key("retry"));
    broker.handle_ack("retry");
}

/// Checks that only overdue records are picked for resending.
#[test]
fn test_collect_retry_candidates_and_resend() {
    let mut broker = test_broker();
    let (id, mut rx) = connect(&mut broker);
    broker.add_subscriber("t", id);
    let timeout = Broker::ACK_TIMEOUT.as_millis() as i64;
    broker.deliver(&record(1, "overdue", 1), 0);
    broker.deliver(&record(2, "recent", 1), timeout);
    frames(&mut rx);

    let (to_resend, to_drop) = broker.collect_retry_candidates(timeout + 1);
    assert_eq!(to_resend, vec!["overdue".to_string()]);
    assert!(to_drop.is_empty());

    assert!(broker.resend_pending_message("overdue", timeout + 1));
    assert_eq!(broker.pending_acks["overdue"].retries, 1);
    assert_eq!(seqs(&frames(&mut rx)), vec![1]);
    assert!(!broker.resend_pending_message("missing", 0));
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
    assert!(shared.with(|broker| broker.add_subscriber("after_panic", id)));
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
        },
    );
    assert!(matches!(result, Err(StorageError::Unsupported(_))));
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
    assert_eq!(
        plan(StartFrom::Seq(2), bounds, None),
        ReplayPlan {
            start: 6,
            truncated: true
        }
    );
    assert_eq!(
        plan(StartFrom::Seq(6), bounds, None),
        ReplayPlan {
            start: 6,
            truncated: false
        }
    );
    assert_eq!(
        plan(StartFrom::Last(8), bounds, None),
        ReplayPlan {
            start: 6,
            truncated: true
        }
    );
    assert_eq!(
        plan(StartFrom::Last(5), bounds, None),
        ReplayPlan {
            start: 6,
            truncated: false
        }
    );
    assert!(!plan(StartFrom::New, bounds, None).truncated);
    assert_eq!(
        plan(StartFrom::SinceMs(0), bounds, Some(6)),
        ReplayPlan {
            start: 6,
            truncated: true
        }
    );
    assert_eq!(
        plan(StartFrom::SinceMs(0), bounds, Some(8)),
        ReplayPlan {
            start: 8,
            truncated: false
        }
    );
    let emptied = LogBounds {
        first_seq: 11,
        last_seq: 10,
    };
    assert_eq!(
        plan(StartFrom::SinceMs(0), emptied, None),
        ReplayPlan {
            start: 11,
            truncated: true
        }
    );
}

/// Checks that a published record is stored with a seq and delivered to a
/// subscriber as a tagged frame, and that a subscriber gets no history by default.
#[tokio::test]
async fn publish_delivers_in_seq_order_and_new_subscribers_get_no_history() {
    let broker = shared(Retention::default());
    let (early, mut early_rx) = broker.with(connect);
    broker
        .subscribe("t", &early, StartFrom::New)
        .await
        .expect("subscribe");
    publish_many(&broker, 3).await;
    assert_eq!(seqs(&frames(&mut early_rx)), vec![1, 2, 3]);

    let (late, mut late_rx) = broker.with(connect);
    let outcome = broker
        .subscribe("t", &late, StartFrom::default())
        .await
        .expect("subscribe");
    assert_eq!(
        (outcome.replayed, outcome.truncated, outcome.added),
        (0, false, true)
    );
    broker
        .publish("t", new_record("m4", "m4"))
        .await
        .expect("publish");
    assert_eq!(seqs(&frames(&mut late_rx)), vec![4]);
    assert_eq!(seqs(&frames(&mut early_rx)), vec![4]);
}

/// Checks that `last` replays the newest records with their original seq and
/// live delivery carries on after them without a repeat.
#[tokio::test]
async fn last_replays_original_seqs_then_live() {
    let broker = shared(Retention::default());
    publish_many(&broker, 5).await;
    let (id, mut rx) = broker.with(connect);
    let outcome = broker
        .subscribe("t", &id, StartFrom::Last(2))
        .await
        .expect("subscribe");
    assert_eq!(outcome.replayed, 2);
    broker
        .publish("t", new_record("m6", "m6"))
        .await
        .expect("publish");
    let received = frames(&mut rx);
    assert_eq!(seqs(&received), vec![4, 5, 6]);
    assert_eq!(received[0]["message_id"], "m4");
}

/// Checks that `since_ms` replays from the first record at or after the time.
#[tokio::test]
async fn since_ms_replays_from_a_time() {
    let broker = shared(Retention::default());
    publish_many(&broker, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let cutoff = chrono::Utc::now().timestamp_millis();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    broker
        .publish("t", new_record("m3", "m3"))
        .await
        .expect("publish");
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::SinceMs(cutoff))
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
    publish_many(&broker, 6).await;
    let (id, mut rx) = broker.with(connect);
    let outcome = broker
        .subscribe("t", &id, StartFrom::Seq(1))
        .await
        .expect("subscribe");
    assert!(outcome.truncated);
    let received = frames(&mut rx);
    assert_eq!(received[0]["type"], "truncated");
    assert_eq!(received[0]["topic"], "t");
    assert_eq!(received[0]["first_available"], 4);
    assert_eq!(seqs(&received), vec![4, 5, 6]);
}

/// Checks that a publisher retrying the same message id gets the original
/// seq back and subscribers receive it once.
#[tokio::test]
async fn a_retried_publish_is_stored_and_delivered_once() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::New)
        .await
        .expect("subscribe");
    let first = broker
        .publish("t", new_record("same", "x"))
        .await
        .expect("publish");
    let again = broker
        .publish("t", new_record("same", "x"))
        .await
        .expect("publish");
    assert!(!first.duplicate);
    assert!(again.duplicate);
    assert_eq!(first.seq, again.seq);
    assert_eq!(seqs(&frames(&mut rx)), vec![1]);
}

/// Checks that a missing message id is replaced with a different UUID each time.
#[tokio::test]
async fn missing_message_ids_are_generated() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::New)
        .await
        .expect("subscribe");
    broker
        .publish("t", new_record("", "a"))
        .await
        .expect("publish");
    broker
        .publish("t", new_record("", "b"))
        .await
        .expect("publish");
    let received = frames(&mut rx);
    assert_eq!(seqs(&received), vec![1, 2]);
    let first = received[0]["message_id"].as_str().expect("id");
    assert_eq!(first.len(), 36);
    assert_ne!(received[0]["message_id"], received[1]["message_id"]);
}

/// Checks that subscribing a client that isn't registered adds nothing.
#[tokio::test]
async fn subscribing_an_unknown_client_adds_nothing() {
    let broker = shared(Retention::default());
    publish_many(&broker, 2).await;
    let outcome = broker
        .subscribe("t", "ghost", StartFrom::Seq(1))
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
    publish_many(&broker, 5).await;
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::Last(10))
        .await
        .expect("subscribe");
    assert_eq!(seqs(&frames(&mut rx)), vec![4, 5]);

    let much_later = chrono::Utc::now().timestamp_millis() + 120_000;
    assert_eq!(broker.trim_all(much_later).await, 2);
    let (other, mut other_rx) = broker.with(connect);
    broker
        .subscribe("t", &other, StartFrom::Seq(1))
        .await
        .expect("subscribe");
    let received = frames(&mut other_rx);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["type"], "truncated");
    assert_eq!(received[0]["first_available"], 6);
}

/// Checks that concurrent publishers to one topic produce one gap-free,
/// ordered stream for a subscriber.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publishers_are_delivered_in_seq_order() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::New)
        .await
        .expect("subscribe");
    let mut tasks = Vec::new();
    for writer in 0..8 {
        let broker = broker.clone();
        tasks.push(tokio::spawn(async move {
            for n in 0..25 {
                broker
                    .publish("t", new_record(&format!("w{writer}-{n}"), "x"))
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
    publish_many(&broker, 50).await;
    let publisher = {
        let broker = broker.clone();
        tokio::spawn(async move {
            for n in 51..=300 {
                broker
                    .publish("t", new_record(&format!("m{n}"), "x"))
                    .await
                    .expect("publish");
            }
        })
    };
    tokio::task::yield_now().await;
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("t", &id, StartFrom::Seq(1))
        .await
        .expect("subscribe");
    publisher.await.expect("publisher task");
    assert_eq!(seqs(&frames(&mut rx)), (1..=300).collect::<Vec<_>>());
}

/// Checks that topics have independent sequences and subscribers.
#[tokio::test]
async fn topics_are_independent() {
    let broker = shared(Retention::default());
    let (id, mut rx) = broker.with(connect);
    broker
        .subscribe("a", &id, StartFrom::New)
        .await
        .expect("subscribe");
    broker
        .publish("b", new_record("x", "x"))
        .await
        .expect("publish");
    broker
        .publish("a", new_record("y", "y"))
        .await
        .expect("publish");
    let received = frames(&mut rx);
    assert_eq!(seqs(&received), vec![1]);
    assert_eq!(received[0]["topic"], "a");
}
