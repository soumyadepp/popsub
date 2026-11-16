//! Unit tests for the broker, using `tokio::sync::mpsc` channels as fake client connections.

use super::Broker;
use popsub_client::Client;
use popsub_config::Settings;
use tokio::sync::mpsc;
use tungstenite::protocol::Message as WsMessage;

/// Checks that a new topic has the given name and no subscribers.
#[test]
fn test_topic_new() {
    let topic = super::topic::Topic::new("test_topic");
    assert_eq!(topic.name, "test_topic");
    assert!(topic.subscribers.is_empty());
}

/// Checks that subscribing adds the id to the topic's subscriber set.
#[test]
fn test_topic_subscribe() {
    let mut topic = super::topic::Topic::new("test_topic");
    topic.subscribe("client1".to_string());
    assert!(topic.subscribers.contains("client1"));
}

/// Checks that unsubscribing removes the id from the topic's subscriber set.
#[test]
fn test_topic_unsubscribe() {
    let mut topic = super::topic::Topic::new("test_topic");
    topic.subscribe("client1".to_string());
    topic.unsubscribe(&"client1".to_string());
    assert!(!topic.subscribers.contains("client1"));
}

/// Checks that a default broker starts with no topics, clients or pending messages.
#[test]
fn test_broker_new() {
    let (broker, _dir) = test_broker();
    assert!(broker.topics.is_empty());
    assert!(broker.clients.is_empty());
    assert!(broker.pending_acks.is_empty());
}

/// Checks that a registered client is stored by id and removed by `remove_client`.
#[test]
fn test_broker_register_and_remove_client() {
    let (mut broker, _dir) = test_broker();
    let (tx, _) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();

    broker.register_client(client);
    assert!(broker.clients.contains_key(&client_id));

    broker.remove_client(&client_id);
    assert!(!broker.clients.contains_key(&client_id));
}

/// Checks that subscribing creates the topic with the client in it and unsubscribing removes the client.
#[test]
fn test_broker_subscribe_and_unsubscribe() {
    let (mut broker, _dir) = test_broker();
    let (tx, _) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);

    broker.subscribe("test_topic", client_id.clone());
    assert!(broker.topics.contains_key("test_topic"));
    let topic = broker.topics.get("test_topic").unwrap();
    assert!(topic.subscribers.contains(&client_id));

    broker.unsubscribe("test_topic", &client_id);
    let topic = broker.topics.get("test_topic").unwrap();
    assert!(!topic.subscribers.contains(&client_id));
}

/// Checks that a QoS 0 publish reaches the subscriber with its topic, payload, id and QoS intact.
#[test]
fn test_broker_publish() {
    let (mut broker, _dir) = test_broker();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id.clone());

    let message = crate::message::Message {
        topic: "test_topic".to_string(),
        payload: "hello".to_string(),
        timestamp: 0,
        message_id: "test_message_id_0".to_string(),
        qos: 0,
    };
    broker.publish(message);

    let received_msg = rx.try_recv().unwrap();
    if let WsMessage::Text(text) = received_msg {
        let received_message: crate::message::Message = serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.topic, "test_topic");
        assert_eq!(received_message.payload, "hello");
        assert_eq!(received_message.message_id, "test_message_id_0");
        assert_eq!(received_message.qos, 0);
    } else {
        panic!("Expected a text message");
    }
}

/// Checks that a QoS 1 publish is delivered, held in `pending_acks`, and removed once acknowledged.
#[test]
fn test_broker_publish_qos1_and_ack() {
    let (mut broker, _dir) = test_broker();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id.clone());

    let message_id = "test_message_id_1".to_string();
    let message = crate::message::Message {
        topic: "test_topic".to_string(),
        payload: "hello_qos1".to_string(),
        timestamp: 0,
        message_id: message_id.clone(),
        qos: 1,
    };
    broker.publish(message.clone());

    assert!(broker.pending_acks.contains_key(&message_id));

    let received_msg = rx.try_recv().unwrap();
    if let WsMessage::Text(text) = received_msg {
        let received_message: crate::message::Message = serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.message_id, message_id);
        assert_eq!(received_message.qos, 1);
    } else {
        panic!("Expected a text message");
    }

    broker.handle_ack(&message_id);

    assert!(!broker.pending_acks.contains_key(&message_id));
}

/// Checks that an ACK for an unknown message id leaves `pending_acks` unchanged.
#[test]
fn test_broker_handle_ack_unknown_message_id() {
    let (mut broker, _dir) = test_broker();
    let initial_pending_acks_count = broker.pending_acks.len();

    broker.handle_ack("unknown_message_id");

    assert_eq!(broker.pending_acks.len(), initial_pending_acks_count);
}

/// Checks that `cleanup_client` removes the client and its subscription.
#[test]
fn test_broker_cleanup_client() {
    let (mut broker, _dir) = test_broker();
    let (tx, _) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id.clone());

    broker.cleanup_client(&client_id);
    assert!(!broker.clients.contains_key(&client_id));
    let topic = broker.topics.get("test_topic").unwrap();
    assert!(!topic.subscribers.contains(&client_id));
}

/// Checks that publishing to a topic with no subscribers does not panic.
#[test]
fn test_publish_to_nonexistent_topic() {
    let (mut broker, _dir) = test_broker();
    let message = crate::message::Message {
        topic: "nonexistent_topic".to_string(),
        payload: "hello".to_string(),
        timestamp: 0,
        message_id: "nonexistent_message".to_string(),
        qos: 0,
    };
    broker.publish(message);
}

/// Checks that publishing to a subscriber whose channel is closed does not panic.
#[test]
fn test_publish_to_client_with_closed_channel() {
    let (mut broker, _dir) = test_broker();
    let (tx, rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id.clone());

    drop(rx);

    let message = crate::message::Message {
        topic: "test_topic".to_string(),
        payload: "hello".to_string(),
        timestamp: 0,
        message_id: "closed_channel_message".to_string(),
        qos: 0,
    };
    broker.publish(message);
}

/// Checks that an unacknowledged QoS 1 message is resent `max_retries` times, then dropped, and that a late ACK for it is ignored.
#[test]
fn test_broker_qos1_retry_mechanism() {
    let settings = Settings::default().broker;
    let step = settings.ack_timeout_ms as i64 + 1;
    let (mut broker, _dir) = test_broker();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id);

    let message_id = "retry_message_id".to_string();
    broker.publish(crate::message::Message {
        topic: "test_topic".to_string(),
        payload: "retry_payload".to_string(),
        timestamp: 0,
        message_id: message_id.clone(),
        qos: 1,
    });
    let sent_at = broker.pending_acks[&message_id].sent_at;
    assert_eq!(received_ids(&mut rx), vec![message_id.clone()]);

    assert_eq!(broker.retry_overdue(sent_at).resent, 0);
    for attempt in 1..=settings.max_retries {
        let outcome = broker.retry_overdue(sent_at + step * attempt as i64);
        assert_eq!(
            outcome,
            crate::RetryOutcome {
                resent: 1,
                dropped: 0
            }
        );
        assert_eq!(received_ids(&mut rx), vec![message_id.clone()]);
        assert_eq!(broker.pending_acks[&message_id].retries, attempt);
    }

    let last = sent_at + step * (settings.max_retries as i64 + 1);
    assert_eq!(
        broker.retry_overdue(last),
        crate::RetryOutcome {
            resent: 0,
            dropped: 1
        }
    );
    assert!(received_ids(&mut rx).is_empty());
    assert!(!broker.pending_acks.contains_key(&message_id));

    broker.handle_ack(&message_id);
    assert!(!broker.pending_acks.contains_key(&message_id));
}

/// Checks that `SharedBroker` keeps working after a task panics while holding its lock.
#[test]
fn shared_broker_recovers_from_a_poisoned_lock() {
    let (broker, _dir) = test_broker();
    let shared = crate::SharedBroker::new(broker);
    let poisoner = shared.clone();
    let result = std::thread::spawn(move || {
        poisoner.with(|_| panic!("simulated panic while holding the broker lock"))
    })
    .join();
    assert!(result.is_err());

    shared.with(|broker| broker.subscribe("after_panic", "client".to_string()));
    assert!(shared.with(|broker| broker.topics.contains_key("after_panic")));
}

/// Drains every frame already queued on a test client's channel and returns their message ids.
///
/// Args:
///     rx: The receiving end of the client's channel.
///
/// Returns:
///     The `message_id` of each queued message, in order.
fn received_ids(rx: &mut mpsc::UnboundedReceiver<WsMessage>) -> Vec<String> {
    let mut ids = Vec::new();
    while let Ok(WsMessage::Text(text)) = rx.try_recv() {
        let message: crate::message::Message = serde_json::from_str(&text).unwrap();
        ids.push(message.message_id);
    }
    ids
}

/// Checks that only overdue messages are picked for resending and that a resend increments the retry count and reaches the subscriber.
#[test]
fn test_collect_retry_candidates_and_resend() {
    let (mut broker, _dir) = test_broker();

    let overdue_id = "overdue".to_string();
    let recent_id = "recent".to_string();

    broker.pending_acks.insert(
        overdue_id.clone(),
        super::engine::PendingMessage {
            message: crate::message::Message {
                topic: "t".to_string(),
                payload: "p".to_string(),
                timestamp: 0,
                message_id: overdue_id.clone(),
                qos: 1,
            },
            sent_at: chrono::Utc::now().timestamp_millis()
                - (Broker::ACK_TIMEOUT.as_millis() as i64 + 1000),
            retries: 0,
        },
    );

    broker.pending_acks.insert(
        recent_id.clone(),
        super::engine::PendingMessage {
            message: crate::message::Message {
                topic: "t".to_string(),
                payload: "p".to_string(),
                timestamp: 0,
                message_id: recent_id.clone(),
                qos: 1,
            },
            sent_at: chrono::Utc::now().timestamp_millis(),
            retries: 0,
        },
    );

    let now = chrono::Utc::now().timestamp_millis();
    let (to_resend, to_drop) = broker.collect_retry_candidates(now);

    assert!(to_resend.contains(&overdue_id));
    assert!(!to_resend.contains(&recent_id));
    assert!(to_drop.is_empty());

    let (mut broker2, _dir2) = test_broker();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker2.register_client(client);
    broker2.subscribe("t", client_id.clone());

    broker2.pending_acks.insert(
        overdue_id.clone(),
        super::engine::PendingMessage {
            message: crate::message::Message {
                topic: "t".to_string(),
                payload: "p".to_string(),
                timestamp: 0,
                message_id: overdue_id.clone(),
                qos: 1,
            },
            sent_at: chrono::Utc::now().timestamp_millis()
                - (Broker::ACK_TIMEOUT.as_millis() as i64 + 1000),
            retries: 0,
        },
    );

    let now = chrono::Utc::now().timestamp_millis();
    let attempted = broker2.resend_pending_message(&overdue_id, now);
    assert!(attempted);
    assert_eq!(broker2.pending_acks.get(&overdue_id).unwrap().retries, 1);

    let received = rx.try_recv().unwrap();
    if let WsMessage::Text(_) = received {
    } else {
        panic!("Expected text message");
    }
}

/// Builds an empty broker whose store lives in a new temporary directory.
///
/// The directory is returned so the caller can keep it alive for as long
/// as the broker uses it.
///
/// Returns:
///     The broker and its temporary directory.
fn test_broker() -> (Broker, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let persistence = popsub_persistence::Persistence::new(
        dir.path().to_str().expect("temp path is not UTF-8"),
        None,
        None,
    )
    .expect("Failed to create persistence");
    (Broker::new(persistence, Settings::default().broker), dir)
}
