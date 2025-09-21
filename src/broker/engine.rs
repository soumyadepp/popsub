use std::collections::HashMap;

use crate::broker::message::Message;
use crate::broker::topic::{SubscriberId, Topic};
use crate::client::Client;
use crate::persistence::sled_store::Persistence;
use tungstenite::protocol::Message as WsMessage;

/// The central broker for managing topics, clients, and message persistence
/// in the Pub/Sub system.
///
/// Responsible for:
/// - Tracking topic-to-subscriber mappings.
/// - Managing connected clients and their channels.
/// - Storing and replaying messages using the `Persistence` layer.
///
/// # Fields
///
/// - `topics` - A map of topic names to `Topic` instances, tracking subscriber lists.
/// - `clients` - A map of subscriber IDs to their corresponding `Client` state (likely includes WebSocket senders).
/// - `persistence` - The message storage backend, supporting TTL and message replay.
///
/// # Example
///
/// ```rust
/// let broker = Broker::default();
/// broker.subscribe("news", subscriber_id, client);
/// ```
#[derive(Debug)]
pub struct Broker {
    pub(crate) topics: HashMap<String, Topic>,
    pub(crate) clients: HashMap<SubscriberId, Client>,
    persistence: Persistence,
}

impl Default for Broker {
    fn default() -> Self {
        if cfg!(test) {
            let dir = tempfile::tempdir().unwrap();
            let persistence = Persistence::new(dir.path().to_str().unwrap(), None, None);
            Self::new_with_persistence(persistence)
        } else {
            Self::new()
        }
    }
}

impl Broker {
    /// Creates a new instance of the broker with empty topics and clients.
    ///
    /// Uses the default configuration for the persistence layer.
    pub fn new() -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            persistence: Persistence::default(),
        }
    }

    /// Creates a new instance of the broker with a specific persistence layer.
    pub fn new_with_persistence(persistence: Persistence) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            persistence,
        }
    }

    /// Registers a client with the broker.
    ///
    /// # Arguments
    ///
    /// * `client` - The client instance containing a unique ID and a message sender channel.
    pub fn register_client(&mut self, client: Client) {
        self.clients.insert(client.id.clone(), client);
    }

    /// Removes a client from the broker using their ID.
    ///
    /// # Arguments
    ///
    /// * `client_id` - The ID of the client to remove.
    pub fn remove_client(&mut self, client_id: &SubscriberId) {
        self.clients.remove(client_id);
    }

    /// Subscribes a client to a given topic. If the topic doesn't exist, it's created.
    ///
    /// On subscription, any recent messages from the persistence layer are replayed to the client.
    ///
    /// # Arguments
    ///
    /// * `topic` - The name of the topic to subscribe to.
    /// * `subscriber` - The subscriber's unique ID.
    pub fn subscribe(&mut self, topic: &str, subscriber: SubscriberId) {
        // Clone before moving
        let subscriber_clone = subscriber.clone();

        let topic = self
            .topics
            .entry(topic.to_string())
            .or_insert_with(|| Topic::new(topic));

        topic.subscribe(subscriber); // move happens here

        if let Some(client) = self.clients.get(&subscriber_clone) {
            let stored_messages = self.persistence.load_messages(topic.name.as_str());
            for stored in stored_messages {
                let replay_msg = Message {
                    topic: stored.topic.clone(),
                    payload: stored.payload.clone(),
                    timestamp: stored.timestamp,
                };
                if let Ok(json) = serde_json::to_string(&replay_msg) {
                    let _ = client.sender.send(WsMessage::text(json));
                }
            }
        }
    }

    /// Unsubscribes a client from a specific topic.
    ///
    /// # Arguments
    ///
    /// * `topic` - The topic name.
    /// * `subscriber` - The ID of the subscriber to remove.
    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) {
        if let Some(t) = self.topics.get_mut(topic) {
            t.unsubscribe(subscriber);
        }
    }

    /// Publishes a message to a topic and broadcasts it to all active subscribers.
    ///
    /// Also stores the message via the persistence layer.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to broadcast.
    pub fn publish(&self, mut msg: Message) {
        msg.timestamp = chrono::Utc::now().timestamp_millis();

        self.persistence.store_message(&msg.topic, &msg.payload);

        if let Some(topic) = self.topics.get(&msg.topic) {
            let text = match serde_json::to_string(&msg) {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("Failed to serialize message: {e}");
                    return;
                }
            };
            let ws_msg = WsMessage::text(text);

            for sub_id in &topic.subscribers {
                if let Some(client) = self.clients.get(sub_id) {
                    if let Err(e) = client.sender.send(ws_msg.clone()) {
                        eprintln!("Failed to send to {sub_id}: {e}");
                    }
                } else {
                    eprintln!("No client registered with id: {sub_id}");
                }
            }
        } else {
            println!("Topic '{}' not found.", msg.topic);
        }
    }

    /// Cleans up a client by removing them and unsubscribing them from all topics.
    ///
    /// # Arguments
    ///
    /// * `client_id` - The ID of the client to remove and unsubscribe.
    pub fn cleanup_client(&mut self, client_id: &SubscriberId) {
        self.remove_client(client_id);

        for (topic, subscribers) in self.topics.iter_mut() {
            subscribers.unsubscribe(client_id);
            println!("Unsubscribed {client_id} from topic {topic}");
        }

        println!("Cleaned up client {client_id}");
    }
}
