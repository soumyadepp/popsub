use super::Broker;
use super::topic::Topic;
use crate::broker::message::Message;
use crate::client::Client;
use tokio::sync::mpsc;
use tungstenite::protocol::Message as WsMessage;

#[test]
fn test_topic_new() {
    let topic = Topic::new("test_topic");
    assert_eq!(topic.name, "test_topic");
    assert!(topic.subscribers.is_empty());
}

#[test]
fn test_topic_subscribe() {
    let mut topic = Topic::new("test_topic");
    topic.subscribe("client1".to_string());
    assert!(topic.subscribers.contains("client1"));
}

#[test]
fn test_topic_unsubscribe() {
    let mut topic = Topic::new("test_topic");
    topic.subscribe("client1".to_string());
    topic.unsubscribe(&"client1".to_string());
    assert!(!topic.subscribers.contains("client1"));
}

#[test]
fn test_broker_new() {
    let broker = Broker::default();
    assert!(broker.topics.is_empty());
    assert!(broker.clients.is_empty());
}

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

#[test]
fn test_broker_publish() {
    let mut broker = Broker::default();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id.clone());

    let message = Message {
        topic: "test_topic".to_string(),
        payload: "hello".to_string(),
        timestamp: 0,
    };
    broker.publish(message);

    let received_msg = rx.try_recv().unwrap();
    if let WsMessage::Text(text) = received_msg {
        let received_message: Message = serde_json::from_str(&text).unwrap();
        assert_eq!(received_message.topic, "test_topic");
        assert_eq!(received_message.payload, "hello");
    } else {
        panic!("Expected a text message");
    }
}

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

#[test]
fn test_publish_to_nonexistent_topic() {
    let broker = Broker::default();
    let message = Message {
        topic: "nonexistent_topic".to_string(),
        payload: "hello".to_string(),
        timestamp: 0,
    };
    broker.publish(message);
    // No assertion, just checking for no panics and that a message is logged.
}

#[test]
fn test_publish_to_client_with_closed_channel() {
    let mut broker = Broker::default();
    let (tx, rx) = mpsc::unbounded_channel::<WsMessage>();
    let client = Client::new(tx);
    let client_id = client.id.clone();
    broker.register_client(client);
    broker.subscribe("test_topic", client_id.clone());

    // Drop the receiver to close the channel
    drop(rx);

    let message = Message {
        topic: "test_topic".to_string(),
        payload: "hello".to_string(),
        timestamp: 0,
    };
    broker.publish(message);

    // No assertion, just checking for no panics and that an error is logged.
}
