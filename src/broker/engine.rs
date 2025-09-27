use std::collections::HashMap;

use crate::broker::message::Message;
use crate::broker::topic::{SubscriberId, Topic};
use crate::client::Client;
use crate::persistence::sled_store::Persistence;
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

/// Represents a message that has been sent and is awaiting acknowledgment.
#[derive(Debug, Clone)]
pub struct PendingMessage {
    pub message: Message,
    pub sent_at: i64,
    pub retries: u8,
}

/// The central broker for managing topics, clients, and message persistence
/// in the Pub/Sub system.
///
/// The `Broker` is the heart of the server, responsible for:
/// - Tracking topic-to-subscriber mappings in memory.
/// - Managing connected clients and their associated WebSocket senders.
/// - Storing messages using a persistence layer to support message replay.
/// - Broadcasting messages to all subscribers of a topic.
/// - Cleaning up client state upon disconnection.
///
/// # Fields
///
/// - `topics`: A `HashMap` where keys are topic names and values are `Topic` instances.
///   This provides fast lookups of topics and their subscriber lists.
/// - `clients`: A `HashMap` that maps unique `SubscriberId`s to their corresponding `Client`
///   structs. This allows the broker to efficiently find and send messages to specific clients.
/// - `persistence`: The storage backend responsible for persisting messages. This enables
///   features like message replay for clients that reconnect.
///
/// # Example
///
/// ```rust
/// use popsub::broker::Broker;
/// use popsub::persistence::sled_store::Persistence;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let persistence = Persistence::new(dir.path().to_str().unwrap(), None, None);
/// let broker = Broker::new_with_persistence(persistence);
/// // The broker can now be used to manage clients and topics.
/// ```
#[derive(Debug)]
pub struct Broker {
    /// A map of topic names to `Topic` instances.
    pub(crate) topics: HashMap<String, Topic>,
    /// A map of subscriber IDs to their corresponding `Client` state.
    pub(crate) clients: HashMap<SubscriberId, Client>,
    /// Messages awaiting acknowledgment for QoS 1.
    pub(crate) pending_acks: HashMap<String, PendingMessage>,
    /// The message storage backend.
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
    /// The maximum number of times a QoS 1 message will be re-sent before being dropped.
    pub const MAX_RETRIES: u8 = 3;

    /// The duration in milliseconds after which an unacknowledged message will be re-sent.
    const ACK_TIMEOUT_MS: i64 = 5000; // 5 seconds

    /// Creates a new instance of the broker with empty topics and clients.
    ///
    /// Uses the default configuration for the persistence layer.
    pub fn new() -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence: Persistence::default(),
        }
    }

    /// Creates a new instance of the broker with a specific persistence layer.
    pub fn new_with_persistence(persistence: Persistence) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
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
                    message_id: Uuid::new_v4().to_string(), // Generate new ID for replayed messages
                    qos: 0,                                 // Replayed messages are QoS 0 for now
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

    /// Publishes a message to a topic, storing it and broadcasting it to all subscribers.
    ///
    /// This method performs two key actions:
    /// 1.  It stores the message in the persistence layer, allowing it to be replayed
    ///     to clients that subscribe or reconnect later.
    /// 2.  It broadcasts the message to all currently subscribed clients of the topic.
    ///
    /// A timestamp is added to the message before it is stored and broadcast.
    ///
    /// # Arguments
    ///
    /// * `msg` - The `Message` to be published. The `topic` field determines where it's sent.
    pub fn publish(&mut self, mut msg: Message) {
        msg.timestamp = chrono::Utc::now().timestamp_millis();

        if msg.message_id.is_empty() {
            msg.message_id = Uuid::new_v4().to_string();
        }

        if msg.qos == 1 {
            self.pending_acks.insert(
                msg.message_id.clone(),
                PendingMessage {
                    message: msg.clone(),
                    sent_at: chrono::Utc::now().timestamp_millis(),
                    retries: 0,
                },
            );
        }

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

    /// Handles an acknowledgement from a client.
    ///
    /// Removes the acknowledged message from the pending_acks.
    pub fn handle_ack(&mut self, message_id: &str) {
        if self.pending_acks.remove(message_id).is_some() {
            println!("ACK received for message_id: {message_id}");
        } else {
            eprintln!("Received ACK for unknown message_id: {message_id}");
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

    /// Starts a background task to re-send unacknowledged messages.
    pub async fn start_retry_loop(broker: std::sync::Arc<std::sync::Mutex<Broker>>) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

            let mut broker_lock = broker.lock().unwrap();
            let mut messages_to_resend = Vec::new();
            let mut messages_to_drop = Vec::new();

            let current_time = chrono::Utc::now().timestamp_millis();

            // Identify messages that have timed out or exceeded max retries
            for (message_id, pending_msg) in &broker_lock.pending_acks {
                if current_time - pending_msg.sent_at > Self::ACK_TIMEOUT_MS {
                    if pending_msg.retries >= Self::MAX_RETRIES {
                        messages_to_drop.push(message_id.clone());
                    } else {
                        messages_to_resend.push(message_id.clone());
                    }
                }
            }

            // Drop messages that exceeded max retries
            for message_id in messages_to_drop {
                if broker_lock.pending_acks.remove(&message_id).is_some() {
                    eprintln!(
                        "Message {} dropped after {} retries.",
                        message_id,
                        Self::MAX_RETRIES
                    );
                }
            }

            // Re-send timed out messages and update their sent_at timestamp and retry count
            for message_id in messages_to_resend {
                if let Some(pending_msg) = broker_lock.pending_acks.get_mut(&message_id) {
                    println!(
                        "Re-sending message {} to subscribers. Retry count: {}",
                        message_id,
                        pending_msg.retries + 1
                    );
                    // Update sent_at timestamp and increment retry count
                    pending_msg.sent_at = current_time;
                    pending_msg.retries += 1;

                    // Re-send the message to all subscribers of the topic
                    let msg_to_resend = pending_msg.message.clone();
                    let topic_name = msg_to_resend.topic.clone();

                    if let Some(topic) = broker_lock.topics.get(&topic_name) {
                        let text = match serde_json::to_string(&msg_to_resend) {
                            Ok(json) => json,
                            Err(e) => {
                                eprintln!("Failed to serialize message for re-send: {e}");
                                continue;
                            }
                        };
                        let ws_msg = WsMessage::text(text);

                        for sub_id in &topic.subscribers {
                            if let Some(client) = broker_lock.clients.get(sub_id) {
                                if let Err(e) = client.sender.send(ws_msg.clone()) {
                                    eprintln!("Failed to re-send to {sub_id}: {e}");
                                }
                            } else {
                                eprintln!("No client registered with id: {sub_id} for re-send");
                            }
                        }
                    }
                }
            }
        }
    }
}
