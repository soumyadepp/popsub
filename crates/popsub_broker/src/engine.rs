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
use std::time::Duration;
use tracing::{error, info, warn};
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

#[derive(Debug, Clone)]
/// A pending QoS=1 message tracked by the broker until an ACK is received.
///
/// Contains the original `Message`, the last `sent_at` timestamp (ms since
/// epoch) and a retry counter used by the retry loop.
pub struct PendingMessage {
    /// The message payload and metadata to be (re-)sent.
    pub message: Message,
    /// When the message was last sent (milliseconds since epoch).
    pub sent_at: i64,
    /// Number of times the broker has retried delivery for this message.
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
    /// ACK timeout as a Duration. Prefer Duration-based APIs internally.
    pub const ACK_TIMEOUT: Duration = Duration::from_millis(5000);
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
    /// Create a new in-memory broker using the default persistence backend.
    ///
    /// The broker starts empty (no topics, clients or pending messages).
    pub fn new() -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence: Persistence::default(),
        }
    }
    /// Create a broker backed by the provided `Persistence` implementation.
    ///
    /// This is useful for tests or when you want to control where messages are stored.
    pub fn new_with_persistence(persistence: Persistence) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence,
        }
    }
    /// Register a newly connected client with the broker.
    ///
    /// The broker will store the client's outgoing sender and use it when
    /// delivering messages to that client.
    pub fn register_client(&mut self, client: Client) {
        self.clients.insert(client.id.clone(), client);
    }
    /// Remove a client from the broker's registry.
    ///
    /// This does not automatically remove subscriptions; use `cleanup_client`
    /// for a full teardown.
    pub fn remove_client(&mut self, client_id: &SubscriberId) {
        self.clients.remove(client_id);
    }
    /// Subscribe `subscriber` to `topic`.
    ///
    /// If the topic does not exist it will be created. If there are stored
    /// messages for the topic they will be replayed to the client (QoS=0
    /// replay semantics).
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

    /// Send a prepared WebSocket message to all subscribers of `topic`.
    ///
    /// Logs send errors but does not remove subscribers; callers may choose
    /// to act on channel closure separately.
    fn send_text_to_subscribers(&self, ws_msg: &WsMessage, topic: &Topic) {
        for sub_id in &topic.subscribers {
            if let Some(client) = self.clients.get(sub_id) {
                if let Err(e) = client.sender.send(ws_msg.clone()) {
                    error!(client = %sub_id, %e, "Failed to send to subscriber");
                }
            } else {
                warn!(client = %sub_id, "No client registered with id");
            }
        }
    }

    /// Collect messages that should be resent or dropped based on `now_ms`.
    ///
    /// Returns a tuple `(to_resend, to_drop)` containing message IDs.
    pub(crate) fn collect_retry_candidates(&self, now_ms: i64) -> (Vec<String>, Vec<String>) {
        let mut to_resend = Vec::new();
        let mut to_drop = Vec::new();

        for (message_id, pending_msg) in &self.pending_acks {
            if now_ms - pending_msg.sent_at > Self::ACK_TIMEOUT_MS {
                if pending_msg.retries >= Self::MAX_RETRIES {
                    to_drop.push(message_id.clone());
                } else {
                    to_resend.push(message_id.clone());
                }
            }
        }

        (to_resend, to_drop)
    }

    /// Resend the pending message with id `message_id` and update its metadata.
    /// Returns true if a resend was attempted, false if the pending message was missing.
    pub(crate) fn resend_pending_message(&mut self, message_id: &str, now_ms: i64) -> bool {
        if let Some(pending_msg) = self.pending_acks.get_mut(message_id) {
            // Update retry metadata
            pending_msg.sent_at = now_ms;
            pending_msg.retries += 1;

            let msg_to_resend = pending_msg.message.clone();
            let topic_name = msg_to_resend.topic.clone();

            if let Some(topic) = self.topics.get(&topic_name) {
                let text = match serde_json::to_string(&msg_to_resend) {
                    Ok(json) => json,
                    Err(e) => {
                        error!(%e, "Failed to serialize message for re-send");
                        return true;
                    }
                };
                let ws_msg = WsMessage::text(text);
                self.send_text_to_subscribers(&ws_msg, topic);
            }

            true
        } else {
            false
        }
    }

    /// Unsubscribe a subscriber from a topic if it exists.
    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) {
        if let Some(t) = self.topics.get_mut(topic) {
            t.unsubscribe(subscriber);
        }
    }
    /// Publish a message to its topic.
    ///
    /// Behavior:
    /// - Set the message timestamp and generate a message_id if missing.
    /// - For QoS=1 messages, track them in `pending_acks` until ACKed.
    /// - Persist the message via the persistence backend.
    /// - Serialize and send the message to all current subscribers of the topic.
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
                    error!(%e, "Failed to serialize message");
                    return;
                }
            };
            let ws_msg = WsMessage::text(text);

            self.send_text_to_subscribers(&ws_msg, topic);
        } else {
            info!(topic = %msg.topic, "Topic not found");
        }
    }

    /// Handle an ACK from a client for the given `message_id`.
    ///
    /// If the message was pending, it is removed from `pending_acks`.
    pub fn handle_ack(&mut self, message_id: &str) {
        if self.pending_acks.remove(message_id).is_some() {
            info!(message_id = %message_id, "ACK received");
        } else {
            warn!(message_id = %message_id, "Received ACK for unknown message_id");
        }
    }
    /// Clean up all resources associated with a client: remove registration
    /// and unsubscribe it from all topics.
    pub fn cleanup_client(&mut self, client_id: &SubscriberId) {
        self.remove_client(client_id);

        for (topic, subscribers) in self.topics.iter_mut() {
            subscribers.unsubscribe(client_id);
            info!(client = %client_id, topic = %topic, "Unsubscribed from topic");
        }

        info!(client = %client_id, "Cleaned up client");
    }

    /// Background retry loop that periodically checks for pending QoS=1
    /// messages that should be retried or dropped.
    ///
    /// This method runs forever and should be spawned as a background task.
    pub async fn start_retry_loop(broker: std::sync::Arc<std::sync::Mutex<Broker>>) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

            let current_time = chrono::Utc::now().timestamp_millis();

            let (messages_to_resend, messages_to_drop) = {
                // Use a scoped borrow to call the read-only helper
                let broker_read = broker.lock().unwrap();
                broker_read.collect_retry_candidates(current_time)
            };

            for message_id in messages_to_drop {
                if let Ok(mut broker_write) = broker.try_lock() {
                    if broker_write.pending_acks.remove(&message_id).is_some() {
                        warn!(message_id = %message_id, retries = %Self::MAX_RETRIES, "Message dropped after max retries");
                    }
                } else {
                    // Fallback: acquire blocking lock
                    let mut broker_write = broker.lock().unwrap();
                    if broker_write.pending_acks.remove(&message_id).is_some() {
                        warn!(message_id = %message_id, retries = %Self::MAX_RETRIES, "Message dropped after max retries");
                    }
                }
            }

            for message_id in messages_to_resend {
                // Resend with a write lock
                let mut broker_write = broker.lock().unwrap();
                let _ = broker_write.resend_pending_message(&message_id, current_time);
            }
        }
    }
}
