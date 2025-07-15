use std::collections::HashMap;

use crate::broker::message::Message;
use crate::broker::topic::{SubscriberId, Topic};
use crate::client::Client;
use tungstenite::protocol::Message as WsMessage;

/// Represents the broker that manages topics and clients
/// It allows clients to subscribe to topics, publish messages, and manage client connections
/// The broker maintains a mapping of topics to subscribers and provides methods for message handling
/// It is responsible for ensuring that messages are delivered to all subscribers of a topic
/// The `Broker` struct is the core of the pub/sub system, enabling communication between clients
/// and managing the state of topics and subscribers.
#[derive(Debug, Default)]
pub struct Broker {
    topics: HashMap<String, Topic>,
    clients: HashMap<SubscriberId, Client>,
}

impl Broker {
    /// Creates a new instance of the Broker
    /// Initializes an empty set of topics and clients
    pub fn new() -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
        }
    }

    /// Registers a new client with the broker
    /// Adds the client to the broker's list of clients
    /// This method is used when a new client connects to the broker
    /// It ensures that the broker keeps track of all active clients
    /// This is essential for the pub/sub functionality, as it allows the broker to send messages
    /// to the correct clients based on their subscriptions
    pub fn register_client(&mut self, client: Client) {
        self.clients.insert(client.id.clone(), client);
    }

    /// Removes a client from the broker
    /// This method is used to clean up clients that are no longer active
    /// It ensures that the broker's state remains consistent and does not hold references to inactive clients
    /// It is essential for maintaining the integrity of the pub/sub system, as it prevents memory leaks
    /// and ensures that only active clients are retained in the broker's state.
    pub fn remove_client(&mut self, client_id: &SubscriberId) {
        self.clients.remove(client_id);
    }

    /// Subscribes a client to a topic. Automatically creates the topic if it doesn't exist.
    pub fn subscribe(&mut self, topic: &str, subscriber: SubscriberId) {
        let topic = self
            .topics
            .entry(topic.to_string())
            .or_insert_with(|| Topic::new(topic));
        topic.subscribe(subscriber);
    }

    /// Unsubscribes a client from a topic
    /// If the topic does not exist, it will not perform any action
    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) {
        if let Some(t) = self.topics.get_mut(topic) {
            t.unsubscribe(subscriber);
        }
    }

    /// Publishes a message to all subscribers of a topic
    /// If the topic does not exist, it will not send the message
    /// This method serializes the message to JSON format before sending it
    /// It handles errors in serialization and sending messages to clients
    /// This is essential for the pub/sub functionality, allowing messages to be sent to all subscribers
    /// of a topic in a structured format
    pub fn publish(&self, msg: Message) {
        if let Some(topic) = self.topics.get(&msg.topic) {
            let text = match serde_json::to_string(&msg) {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("Failed to serialize message: {:?}", e);
                    return;
                }
            };
            let ws_msg = WsMessage::text(text);
            for sub_id in &topic.subscribers {
                if let Some(client) = self.clients.get(sub_id) {
                    if let Err(e) = client.sender.send(ws_msg.clone()) {
                        eprintln!("Failed to send to {}: {}", sub_id, e);
                    }
                } else {
                    eprintln!("No client registered with id: {}", sub_id);
                }
            }
        } else {
            println!("Topic '{}' not found.", msg.topic);
        }
    }

    /// Cleans up a client by removing it and unsubscribing it from all topics
    /// This is useful for when a client disconnects or is no longer active
    /// It ensures that the broker's state remains consistent and does not hold references to inactive clients
    pub fn cleanup_client(&mut self, client_id: &SubscriberId) {
        self.remove_client(client_id);

        for (topic, subscribers) in self.topics.iter_mut() {
            subscribers.unsubscribe(client_id);
            println!("Unsubscribed {} from topic {}", client_id, topic);
        }

        println!("Cleaned up client {}", client_id);
    }
}
