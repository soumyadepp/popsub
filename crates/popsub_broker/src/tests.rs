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
    let broker = Broker::default();
    assert!(broker.topics.is_empty());
    assert!(broker.clients.is_empty());
    assert!(broker.pending_acks.is_empty());
}

/// Checks that a registered client is stored by id and removed by `remove_client`.
#[test]
fn test_broker_register_and_remove_client() {
    let mut broker = Broker::default();
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
    let mut broker = Broker::default();
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
    let mut broker = Broker::default();
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
    let mut broker = Broker::default();
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
    let mut broker = Broker::default();
    let initial_pending_acks_count = broker.pending_acks.len();

    broker.handle_ack("unknown_message_id");

    assert_eq!(broker.pending_acks.len(), initial_pending_acks_count);
}

/// Checks that `cleanup_client` removes the client and its subscription.
#[test]
fn test_broker_cleanup_client() {
    let mut broker = Broker::default();
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
    let mut broker = Broker::default();
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
    let mut broker = Broker::default();
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
#[tokio::test]
async fn test_broker_qos1_retry_mechanism() {
    let broker_arc = std::sync::Arc::new(std::sync::Mutex::new(Broker::default()));
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();

    {
        let mut broker = broker_arc.lock().unwrap();
        broker.register_client(client);
        broker.subscribe("test_topic", client_id.clone());
    }

    let message_id = "retry_message_id".to_string();
    let message = crate::message::Message {
        topic: "test_topic".to_string(),
        payload: "retry_payload".to_string(),
        timestamp: 0,
        message_id: message_id.clone(),
        qos: 1,
    };

    {
        let mut broker = broker_arc.lock().unwrap();
        broker.publish(message.clone());
    }

    assert!(
        broker_arc
            .lock()
            .unwrap()
            .pending_acks
            .contains_key(&message_id)
    );

    let received_msg = rx.recv().await.unwrap();
    if let WsMessage::Text(text) = received_msg {
        let received_message: crate::message::Message = serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.message_id, message_id);
        assert_eq!(received_message.qos, 1);
    } else {
        panic!("Expected a text message");
    }

    for i in 0..Settings::default().broker.max_retries {
        tokio::time::sleep(tokio::time::Duration::from_millis(5100)).await;
        tokio::spawn(Broker::start_retry_loop(broker_arc.clone()));

        let re_sent_msg = rx.recv().await.unwrap();
        if let WsMessage::Text(text) = re_sent_msg {
            let re_sent_message: crate::message::Message = serde_json::from_str(&text).unwrap();
            assert_eq!(re_sent_message.message_id, message_id);
            assert_eq!(re_sent_message.qos, 1);
            assert_eq!(
                broker_arc
                    .lock()
                    .unwrap()
                    .pending_acks
                    .get(&message_id)
                    .unwrap()
                    .retries,
                i + 1
            );
        } else {
            panic!("Expected a text message on re-send");
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(5100)).await;
    tokio::spawn(Broker::start_retry_loop(broker_arc.clone()));

    assert!(rx.try_recv().is_err());

    assert!(
        !broker_arc
            .lock()
            .unwrap()
            .pending_acks
            .contains_key(&message_id)
    );

    {
        let mut broker = broker_arc.lock().unwrap();
        broker.handle_ack(&message_id);
    }
    assert!(
        !broker_arc
            .lock()
            .unwrap()
            .pending_acks
            .contains_key(&message_id)
    );
}

/// Checks that only overdue messages are picked for resending and that a resend increments the retry count and reaches the subscriber.
#[test]
fn test_collect_retry_candidates_and_resend() {
    let mut broker = Broker::default();

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

    let mut broker2 = Broker::default();
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
