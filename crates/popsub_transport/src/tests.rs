pub use crate::message::ClientMessage;
pub use popsub_broker::Broker;
use popsub_persistence::Persistence;
pub use serde_json::json;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
pub use tokio::sync::mpsc;
use uuid::Uuid;

// This is a helper function that simulates the message handling part of the websocket server.
async fn handle_message(broker: Arc<Mutex<Broker>>, client_id: String, msg: String) {
    match serde_json::from_str::<ClientMessage>(&msg) {
        Ok(ClientMessage::Auth { .. }) => {
            // Auth not handled in this test helper
        }
        Ok(ClientMessage::Login { .. }) => {
            // Login not handled in this test helper
        }
        Ok(ClientMessage::Subscribe { topic }) => {
            let mut broker = broker.lock().unwrap();
            broker.subscribe(&topic, client_id.clone());
        }
        Ok(ClientMessage::Unsubscribe { topic }) => {
            let mut broker = broker.lock().unwrap();
            broker.unsubscribe(&topic, &client_id);
        }
        Ok(ClientMessage::Publish {
            topic,
            payload,
            message_id,
            qos,
        }) => {
            let mut broker = broker.lock().unwrap();
            let timestamp = chrono::Utc::now().timestamp_millis();
            broker.publish(popsub_broker::message::Message {
                topic: topic.clone(),
                payload,
                timestamp,
                message_id: message_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                qos: qos.unwrap_or(0),
            });
        }
        Ok(ClientMessage::Ack { message_id }) => {
            let mut broker = broker.lock().unwrap();
            broker.handle_ack(&message_id);
        }
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_handle_subscribe() {
    let tmp = tempdir().unwrap();
    let persistence = Persistence::new(tmp.path().to_str().unwrap(), None, None);
    let broker = Arc::new(Mutex::new(Broker::new_with_persistence(persistence)));
    let client_id = "test_client".to_string();

    let msg = json!({
        "type": "subscribe",
        "topic": "test_topic"
    })
    .to_string();

    handle_message(broker.clone(), client_id.clone(), msg).await;

    let broker = broker.lock().unwrap();
    let topic = broker.topics.get("test_topic").unwrap();
    assert!(topic.subscribers.contains(&client_id));
}

#[tokio::test]
async fn test_handle_unsubscribe() {
    let tmp = tempdir().unwrap();
    let persistence = Persistence::new(tmp.path().to_str().unwrap(), None, None);
    let broker = Arc::new(Mutex::new(Broker::new_with_persistence(persistence)));
    let client_id = "test_client".to_string();

    // First, subscribe the client to the topic
    broker
        .lock()
        .unwrap()
        .subscribe("test_topic", client_id.clone());

    let msg = json!({
        "type": "unsubscribe",
        "topic": "test_topic"
    })
    .to_string();

    handle_message(broker.clone(), client_id.clone(), msg).await;

    let broker = broker.lock().unwrap();
    let topic = broker.topics.get("test_topic").unwrap();
    assert!(!topic.subscribers.contains(&client_id));
}

#[tokio::test]
async fn test_handle_publish() {
    let tmp = tempdir().unwrap();
    let persistence = Persistence::new(tmp.path().to_str().unwrap(), None, None);
    let broker = Arc::new(Mutex::new(Broker::new_with_persistence(persistence)));
    let client_id = "test_client".to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = popsub_client::Client::new(tx);
    let client_id_for_broker = client.id.clone();
    broker.lock().unwrap().register_client(client);
    broker
        .lock()
        .unwrap()
        .subscribe("test_topic", client_id_for_broker.clone());

    let msg = json!({
        "type": "publish",
        "topic": "test_topic",
        "payload": "hello",
        "message_id": "publish_message_id",
        "qos": 0
    })
    .to_string();

    handle_message(broker.clone(), client_id.clone(), msg).await;

    let received_msg = rx.try_recv().unwrap();
    if let tungstenite::protocol::Message::Text(text) = received_msg {
        let received_message: popsub_broker::message::Message =
            serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.topic, "test_topic");
        assert_eq!(received_message.payload, "hello");
        assert_eq!(received_message.message_id, "publish_message_id");
        assert_eq!(received_message.qos, 0);
    } else {
        panic!("Expected a text message");
    }
}

#[tokio::test]
async fn test_handle_ack() {
    let tmp = tempdir().unwrap();
    let persistence = Persistence::new(tmp.path().to_str().unwrap(), None, None);
    let broker = Arc::new(Mutex::new(Broker::new_with_persistence(persistence)));
    let client_id = "test_client".to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = popsub_client::Client::new(tx);
    let client_id_for_broker = client.id.clone();
    broker.lock().unwrap().register_client(client);
    broker
        .lock()
        .unwrap()
        .subscribe("test_topic", client_id_for_broker.clone());

    let message_id = "ack_message_id".to_string();
    let publish_msg = json!({
        "type": "publish",
        "topic": "test_topic",
        "payload": "hello_qos1",
        "message_id": message_id,
        "qos": 1
    })
    .to_string();

    handle_message(broker.clone(), client_id.clone(), publish_msg).await;

    // Assert that the message is in pending_acks
    assert!(
        broker
            .lock()
            .unwrap()
            .pending_acks
            .contains_key("ack_message_id")
    );

    // Simulate client receiving and acknowledging the message
    let received_msg = rx.try_recv().unwrap();
    if let tungstenite::protocol::Message::Text(text) = received_msg {
        let received_message: popsub_broker::message::Message =
            serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.message_id, "ack_message_id");
        assert_eq!(received_message.qos, 1);
    } else {
        panic!("Expected a text message");
    }

    let ack_msg = json!({
        "type": "ack",
        "message_id": "ack_message_id"
    })
    .to_string();

    handle_message(broker.clone(), client_id.clone(), ack_msg).await;

    // Assert that the message is removed from pending_acks
    assert!(
        !broker
            .lock()
            .unwrap()
            .pending_acks
            .contains_key("ack_message_id")
    );
}

#[tokio::test]
async fn test_send_loop_forwards_message() {
    let (tx, mut rx) = mpsc::unbounded_channel::<tungstenite::protocol::Message>();
    let (cap_tx, cap_rx) = std::sync::mpsc::sync_channel::<String>(1);

    // Spawn forwarder similar to the send loop — forwards text messages into cap_tx
    std::thread::spawn(move || {
        // synchronous thread loop reading from tokio mpsc via a small runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            while let Some(msg) = rx.recv().await {
                if msg.is_text() {
                    let _ = cap_tx.send(msg.to_text().unwrap().to_string());
                }
            }
        });
    });

    tx.send(tungstenite::protocol::Message::Text("hello".into()))
        .unwrap();

    let received = cap_rx.recv().expect("should receive message");
    assert_eq!(received, "hello");
}
