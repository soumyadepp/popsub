use std::collections::HashMap;

use crate::broker::message::Message;
use crate::broker::topic::{SubscriberId, Topic};
use crate::client::Client;
use tungstenite::protocol::Message as WsMessage;

#[derive(Debug, Default)]
pub struct Broker {
    topics: HashMap<String, Topic>,
    clients: HashMap<SubscriberId, Client>,
}

impl Broker {
    pub fn new() -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
        }
    }

    /// Registers a new client with the broker
    pub fn register_client(&mut self, client: Client) {
        self.clients.insert(client.id.clone(), client);
    }

    /// Removes a client from the broker
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
    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) {
        if let Some(t) = self.topics.get_mut(topic) {
            t.unsubscribe(subscriber);
        }
    }

    /// Publishes a message to all subscribers of a topic
    pub fn publish(&self, msg: Message) {
        if let Some(topic) = self.topics.get(&msg.topic) {
            let text = serde_json::to_string(&msg).unwrap_or_default();
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
}
