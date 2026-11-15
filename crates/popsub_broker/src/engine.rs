//! The `Broker` engine: topic routing, delivery to client channels, QoS 1 retries and replay.
//!
//! The broker is synchronous and has no locking of its own. The server keeps
//! one `Arc<Mutex<Broker>>` and every connection task locks it for each call,
//! so callers must not hold that lock across an `.await`. Delivery pushes
//! `tungstenite` messages into each client's unbounded channel, so no call
//! here waits on the network. Calls that touch persistence (`publish` and
//! `subscribe`) do disk work while the caller holds the lock.
//!
//! QoS 1 messages are kept in `pending_acks`, keyed by message id only. One
//! ACK from any subscriber clears the message for all of them, and every
//! retry goes to every current subscriber of the topic. `start_retry_loop`
//! is the background task that resends unacknowledged messages and drops
//! them after `max_retries` resends.

use popsub_config::Settings;
use std::collections::HashMap;

use crate::message::Message;
use crate::topic::{SubscriberId, Topic};
use popsub_client::Client;
use popsub_config::settings::BrokerSettings;
use popsub_persistence::Persistence;
use std::time::Duration;
use tracing::{error, info, warn};
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

/// A QoS 1 message the broker keeps until it is acknowledged or dropped.
///
/// There is one entry per message id, shared by all subscribers of the topic.
#[derive(Debug, Clone)]
pub struct PendingMessage {
    /// The message as it was published, resent unchanged on each retry.
    pub message: Message,
    /// When the message was last sent, in milliseconds since the UNIX epoch.
    pub sent_at: i64,
    /// How many times the retry loop has resent the message.
    pub retries: u8,
}

/// The in-memory broker: topics, connected clients and unacknowledged QoS 1 messages.
///
/// All methods take `&self` or `&mut self` and never block on the network.
/// The server shares one instance behind a mutex; see the module docs for
/// what that implies.
#[derive(Debug)]
pub struct Broker {
    /// Topics by name. A topic is created on its first subscribe and never removed.
    pub topics: HashMap<String, Topic>,
    /// Connected clients by id, used to reach each subscriber's channel.
    pub clients: HashMap<SubscriberId, Client>,
    /// Unacknowledged QoS 1 messages by message id, shared by all subscribers.
    pub pending_acks: HashMap<String, PendingMessage>,
    /// Store that published messages are written to and replayed from.
    persistence: Persistence,
    /// ACK timeout and retry limit used by the retry loop.
    settings: BrokerSettings,
}

impl Broker {
    /// Default ACK timeout of 5000 ms.
    ///
    /// Only tests read this. The retry loop uses `settings.ack_timeout_ms`,
    /// so the two can differ if either default changes.
    pub const ACK_TIMEOUT: Duration = Duration::from_millis(5000);
}

impl Default for Broker {
    /// Creates a broker with the default broker settings.
    ///
    /// In test builds the store is opened in a fresh temporary directory. The
    /// `TempDir` handle is dropped when this function returns, so the
    /// directory is removed while the store still has it open. Outside tests
    /// this calls `Broker::new`, which opens `pubsub_db` in the working
    /// directory. Panics if the temporary directory or the store can't be
    /// created.
    ///
    /// Returns:
    ///     An empty broker.
    fn default() -> Self {
        if cfg!(test) {
            let dir = tempfile::tempdir().expect("Failed to create temp dir");
            let persistence =
                Persistence::new(dir.path().to_str().expect("path to_str failed"), None, None)
                    .expect("Failed to create persistence");
            Self::new_with_persistence(persistence, Settings::default().broker)
        } else {
            Self::new(Settings::default().broker)
        }
    }
}

impl Broker {
    /// Creates an empty broker backed by the default persistence store.
    ///
    /// The store is `Persistence::default()`, which always opens `pubsub_db`
    /// in the working directory with its own TTL, so `message_ttl_secs` from
    /// the settings is not applied. Panics if the store can't be opened.
    ///
    /// Args:
    ///     settings: ACK timeout and retry limit for QoS 1 messages.
    ///
    /// Returns:
    ///     A broker with no topics, clients or pending messages.
    pub fn new(settings: BrokerSettings) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence: Persistence::default(),
            settings,
        }
    }

    /// Creates an empty broker backed by the given persistence store.
    ///
    /// Use this in tests or wherever the store's path and limits need to be chosen.
    ///
    /// Args:
    ///     persistence: The store to write published messages to and replay from.
    ///     settings: ACK timeout and retry limit for QoS 1 messages.
    ///
    /// Returns:
    ///     A broker with no topics, clients or pending messages.
    pub fn new_with_persistence(persistence: Persistence, settings: BrokerSettings) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            persistence,
            settings,
        }
    }

    /// Registers a connected client so messages can be delivered to its channel.
    ///
    /// A client registered under an existing id replaces the old entry.
    ///
    /// Args:
    ///     client: The client, keyed by its `id`.
    pub fn register_client(&mut self, client: Client) {
        self.clients.insert(client.id.clone(), client);
    }

    /// Removes a client from the registry.
    ///
    /// Its subscriptions stay in place; use `cleanup_client` to remove those too.
    ///
    /// Args:
    ///     client_id: The id of the client to remove.
    pub fn remove_client(&mut self, client_id: &SubscriberId) {
        self.clients.remove(client_id);
    }

    /// Subscribes a client to a topic and replays the topic's stored history to it.
    ///
    /// The topic is created if it does not exist. If the client is registered,
    /// every message the store holds for the topic is sent to it as QoS 0 with
    /// a fresh message id and the timestamp the store recorded. The replay
    /// happens on every call, including when the client was already
    /// subscribed. The store is read while the caller holds the broker lock.
    /// A failed read is logged and failed sends are ignored.
    ///
    /// Args:
    ///     topic: The topic name.
    ///     subscriber: The id of the client to subscribe.
    pub fn subscribe(&mut self, topic: &str, subscriber: SubscriberId) {
        let subscriber_clone = subscriber.clone();

        let topic = self
            .topics
            .entry(topic.to_string())
            .or_insert_with(|| Topic::new(topic));

        topic.subscribe(subscriber);

        if let Some(client) = self.clients.get(&subscriber_clone) {
            match self.persistence.load_messages(topic.name.as_str()) {
                Ok(stored_messages) => {
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
                Err(e) => {
                    error!("Failed to load messages for topic {}: {}", topic.name, e);
                }
            }
        }
    }

    /// Sends a WebSocket frame to every subscriber of a topic.
    ///
    /// A send to a closed channel is logged, and a subscriber id with no
    /// registered client is logged as a warning. Neither removes the
    /// subscriber; that happens in `cleanup_client`.
    ///
    /// Args:
    ///     ws_msg: The frame to send. It is cloned for each subscriber.
    ///     topic: The topic whose subscribers receive the frame.
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

    /// Finds pending messages whose ACK timeout has passed.
    ///
    /// A message is due once more than `settings.ack_timeout_ms` has passed
    /// since it was last sent. Due messages that have already been resent
    /// `settings.max_retries` times are marked for dropping; the rest are
    /// marked for resending. Nothing is changed.
    ///
    /// Args:
    ///     now_ms: The current time in milliseconds since the UNIX epoch.
    ///
    /// Returns:
    ///     A pair `(to_resend, to_drop)` of message ids.
    pub(crate) fn collect_retry_candidates(&self, now_ms: i64) -> (Vec<String>, Vec<String>) {
        let mut to_resend = Vec::new();
        let mut to_drop = Vec::new();

        for (message_id, pending_msg) in &self.pending_acks {
            if now_ms - pending_msg.sent_at > self.settings.ack_timeout_ms as i64 {
                if pending_msg.retries >= self.settings.max_retries {
                    to_drop.push(message_id.clone());
                } else {
                    to_resend.push(message_id.clone());
                }
            }
        }

        (to_resend, to_drop)
    }

    /// Resends a pending message to every current subscriber of its topic.
    ///
    /// The entry's `sent_at` is set to `now_ms` and its retry count is
    /// incremented before sending. Subscribers that joined after the original
    /// publish receive the retry too. If the topic no longer exists, or the
    /// message fails to serialise, nothing is sent but the retry still counts.
    ///
    /// Args:
    ///     message_id: The id of the pending message.
    ///     now_ms: The current time in milliseconds since the UNIX epoch.
    ///
    /// Returns:
    ///     `true` if the message was pending, `false` if it was not found.
    pub(crate) fn resend_pending_message(&mut self, message_id: &str, now_ms: i64) -> bool {
        if let Some(pending_msg) = self.pending_acks.get_mut(message_id) {
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

    /// Removes a client from a topic's subscribers.
    ///
    /// Does nothing if the topic does not exist. The topic itself is kept
    /// even when it has no subscribers left.
    ///
    /// Args:
    ///     topic: The topic name.
    ///     subscriber: The id of the client to remove.
    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) {
        if let Some(t) = self.topics.get_mut(topic) {
            t.unsubscribe(subscriber);
        }
    }

    /// Publishes a message to every current subscriber of its topic.
    ///
    /// The timestamp is overwritten with the current time and an empty
    /// `message_id` is replaced with a new UUID. A QoS 1 message is added to
    /// `pending_acks` before delivery, even when the topic has no
    /// subscribers. The payload is then written to the persistence store
    /// while the caller holds the broker lock, so a slow disk delays every
    /// other broker call. A store failure is logged and delivery goes ahead.
    /// Publishing to a topic nobody has subscribed to stores the message and
    /// logs "Topic not found".
    ///
    /// Args:
    ///     msg: The message to publish.
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

        if let Err(e) = self.persistence.store_message(&msg.topic, &msg.payload) {
            error!("Failed to store message: {}", e);
        }

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

    /// Removes a QoS 1 message from `pending_acks` when a client acknowledges it.
    ///
    /// Pending entries are keyed by message id only and the sender is not
    /// checked, so the first ACK from any client stops retries for every
    /// subscriber. An ACK for an unknown id is logged and ignored.
    ///
    /// Args:
    ///     message_id: The id from the client's ACK.
    pub fn handle_ack(&mut self, message_id: &str) {
        if self.pending_acks.remove(message_id).is_some() {
            info!(message_id = %message_id, "ACK received");
        } else {
            warn!(message_id = %message_id, "Received ACK for unknown message_id");
        }
    }

    /// Removes a disconnected client and unsubscribes it from every topic.
    ///
    /// Pending QoS 1 messages are not touched, and topics left with no
    /// subscribers are kept.
    ///
    /// Args:
    ///     client_id: The id of the client to remove.
    pub fn cleanup_client(&mut self, client_id: &SubscriberId) {
        self.remove_client(client_id);

        for (topic, subscribers) in self.topics.iter_mut() {
            subscribers.unsubscribe(client_id);
            info!(client = %client_id, topic = %topic, "Unsubscribed from topic");
        }

        info!(client = %client_id, "Cleaned up client");
    }

    /// Resends or drops overdue QoS 1 messages once a second, forever.
    ///
    /// Spawn this as a background task. Each tick it locks the broker to
    /// collect overdue messages, then locks it again for each message to drop
    /// and each message to resend, so other callers can run in between. A
    /// message is sent at most `1 + max_retries` times before it is dropped.
    /// The lock is a `std::sync::Mutex` taken from async code, so a tick
    /// blocks its worker thread while another caller holds the lock. Panics
    /// if the lock is poisoned.
    ///
    /// Args:
    ///     broker: The shared broker to retry messages on.
    pub async fn start_retry_loop(broker: std::sync::Arc<std::sync::Mutex<Broker>>) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

            let current_time = chrono::Utc::now().timestamp_millis();

            let (messages_to_resend, messages_to_drop) = {
                let broker_read = broker.lock().expect("Broker lock poisoned");
                broker_read.collect_retry_candidates(current_time)
            };

            for message_id in messages_to_drop {
                if let Ok(mut broker_write) = broker.try_lock() {
                    if broker_write.pending_acks.remove(&message_id).is_some() {
                        warn!(message_id = %message_id, retries = %broker_write.settings.max_retries, "Message dropped after max retries");
                    }
                } else {
                    let mut broker_write = broker.lock().expect("Broker lock poisoned");
                    if broker_write.pending_acks.remove(&message_id).is_some() {
                        warn!(message_id = %message_id, retries = %broker_write.settings.max_retries, "Message dropped after max retries");
                    }
                }
            }

            for message_id in messages_to_resend {
                let mut broker_write = broker.lock().expect("Broker lock poisoned");
                let _ = broker_write.resend_pending_message(&message_id, current_time);
            }
        }
    }
}
