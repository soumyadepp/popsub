use crate::broker::Broker;
use crate::transport::message::ClientMessage;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// This is a helper function that simulates the message handling part of the websocket server.
async fn handle_message(broker: Arc<Mutex<Broker>>, client_id: String, msg: String) {
    match serde_json::from_str::<ClientMessage>(&msg) {
        Ok(ClientMessage::Subscribe { topic }) => {
            let mut broker = broker.lock().unwrap();
            broker.subscribe(&topic, client_id.clone());
        }
        Ok(ClientMessage::Unsubscribe { topic }) => {
            let mut broker = broker.lock().unwrap();
            broker.unsubscribe(&topic, &client_id);
        }
        Ok(ClientMessage::Publish { topic, payload }) => {
            let broker = broker.lock().unwrap();
            let timestamp = chrono::Utc::now().timestamp_millis();
            broker.publish(crate::broker::message::Message {
                topic: topic.clone(),
                payload,
                timestamp,
            });
        }
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_handle_subscribe() {
    let broker = Arc::new(Mutex::new(Broker::default()));
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
    let broker = Arc::new(Mutex::new(Broker::default()));
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
    let broker = Arc::new(Mutex::new(Broker::default()));
    let client_id = "test_client".to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = crate::client::Client::new(tx);
    let client_id_for_broker = client.id.clone();
    broker.lock().unwrap().register_client(client);
    broker
        .lock()
        .unwrap()
        .subscribe("test_topic", client_id_for_broker.clone());

    let msg = json!({
        "type": "publish",
        "topic": "test_topic",
        "payload": "hello"
    })
    .to_string();

    handle_message(broker.clone(), client_id.clone(), msg).await;

    let received_msg = rx.try_recv().unwrap();
    if let tungstenite::protocol::Message::Text(text) = received_msg {
        let received_message: crate::broker::message::Message =
            serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.topic, "test_topic");
        assert_eq!(received_message.payload, "hello");
    } else {
        panic!("Expected a text message");
    }
}
