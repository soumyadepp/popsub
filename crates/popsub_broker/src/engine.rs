//! Broker engine
//!
//! This module contains the in-memory broker implementation responsible for:
//! - managing topics and subscriber lists
//! - publishing messages to subscribers
//! - tracking QoS=1 pending messages and retrying until acknowledged
//! - persisting messages for replay via the `Persistence` abstraction
//!
//! Concurrency and usage notes:
//! - The public API here is synchronous and designed to be held behind a
//!   lock (for example `Arc<Mutex<Broker>>`) by the transport layer. Callers
//!   should avoid holding the broker lock across network I/O to prevent
//!   blocking other operations.
//! - The retry loop is designed to be run as a background task and will
//!   re-send pending QoS=1 messages when ACKs are not received within a
//!   configured timeout. Retries are capped to avoid infinite resend loops.

use std::collections::HashMap;

use crate::message::Message;
use crate::topic::{SubscriberId, Topic};
use popsub_client::Client;
use popsub_persistence::Persistence;
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PendingMessage {
    pub message: Message,
    pub sent_at: i64,
    pub retries: u8,
}

#[derive(Debug)]
pub struct Broker {
    pub topics: HashMap<String, Topic>,
    pub clients: HashMap<SubscriberId, Client>,
    pub pending_acks: HashMap<String, PendingMessage>,
    persistence: Persistence,
}

impl Broker {
    /// Maximum number of delivery retries for QoS=1 messages before dropping.
    ///
    /// This prevents endless redelivery loops for clients that never ACK.
    pub const MAX_RETRIES: u8 = 5;

    /// Timeout in milliseconds after which an un-ACKed message is considered
    /// eligible for retry. The retry loop checks this periodically.
    const ACK_TIMEOUT_MS: i64 = 5000;
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
    // constants are defined earlier on the type; keep implementation block focused on behavior

    pub fn new() -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence: Persistence::default(),
        }
    }

    pub fn new_with_persistence(persistence: Persistence) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence,
        }
    }

    pub fn register_client(&mut self, client: Client) {
        self.clients.insert(client.id.clone(), client);
    }

    pub fn remove_client(&mut self, client_id: &SubscriberId) {
        self.clients.remove(client_id);
    }

    pub fn subscribe(&mut self, topic: &str, subscriber: SubscriberId) {
        let subscriber_clone = subscriber.clone();

        let topic = self
            .topics
            .entry(topic.to_string())
            .or_insert_with(|| Topic::new(topic));

        topic.subscribe(subscriber);

        if let Some(client) = self.clients.get(&subscriber_clone) {
            let stored_messages = self.persistence.load_messages(topic.name.as_str());
            for stored in stored_messages {
                let replay_msg = Message {
                    topic: stored.topic.clone(),
                    payload: stored.payload.clone(),
                    timestamp: stored.timestamp,
                    message_id: Uuid::new_v4().to_string(),
                    qos: 0,
                };
                if let Ok(json) = serde_json::to_string(&replay_msg) {
                    let _ = client.sender.send(WsMessage::text(json));
                }
            }
        }
    }

    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) {
        if let Some(t) = self.topics.get_mut(topic) {
            t.unsubscribe(subscriber);
        }
    }

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

    pub fn handle_ack(&mut self, message_id: &str) {
        if self.pending_acks.remove(message_id).is_some() {
            println!("ACK received for message_id: {message_id}");
        } else {
            eprintln!("Received ACK for unknown message_id: {message_id}");
        }
    }

    pub fn cleanup_client(&mut self, client_id: &SubscriberId) {
        self.remove_client(client_id);

        for (topic, subscribers) in self.topics.iter_mut() {
            subscribers.unsubscribe(client_id);
            println!("Unsubscribed {client_id} from topic {topic}");
        }

        println!("Cleaned up client {client_id}");
    }

    pub async fn start_retry_loop(broker: std::sync::Arc<std::sync::Mutex<Broker>>) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

            let mut broker_lock = broker.lock().unwrap();
            let mut messages_to_resend = Vec::new();
            let mut messages_to_drop = Vec::new();

            let current_time = chrono::Utc::now().timestamp_millis();

            for (message_id, pending_msg) in &broker_lock.pending_acks {
                if current_time - pending_msg.sent_at > Self::ACK_TIMEOUT_MS {
                    if pending_msg.retries >= Self::MAX_RETRIES {
                        messages_to_drop.push(message_id.clone());
                    } else {
                        messages_to_resend.push(message_id.clone());
                    }
                }
            }

            for message_id in messages_to_drop {
                if broker_lock.pending_acks.remove(&message_id).is_some() {
                    eprintln!(
                        "Message {} dropped after {} retries.",
                        message_id,
                        Self::MAX_RETRIES
                    );
                }
            }

            for message_id in messages_to_resend {
                if let Some(pending_msg) = broker_lock.pending_acks.get_mut(&message_id) {
                    println!(
                        "Re-sending message {} to subscribers. Retry count: {}",
                        message_id,
                        pending_msg.retries + 1
                    );
                    pending_msg.sent_at = current_time;
                    pending_msg.retries += 1;

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
