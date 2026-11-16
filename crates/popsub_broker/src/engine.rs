//! The `Broker` engine: connected clients, topic subscriber sets, fan-out to
//! client channels, and the QoS 1 pending table and retries.
//!
//! The broker is synchronous, has no locking of its own and does no I/O
//! beyond pushing `tungstenite` messages into unbounded channels, so nothing
//! here waits. Storage is not reached from here: each topic's writer (see
//! `writer`) appends to the log without the lock, then calls `deliver` under
//! it. The server shares the broker through `SharedBroker`, whose `with`
//! runs a closure under the lock, so the lock can't be held across an
//! `.await`.
//!
//! QoS 1 records are kept in `pending_acks`, keyed by message id only. One
//! ACK from any subscriber clears the record for all of them, and every
//! retry goes to every current subscriber of the topic. Replayed records are
//! never added to it. `retry_overdue` resends unacknowledged records and
//! drops them after `max_retries` resends; `SharedBroker::run_retry_loop`
//! calls it on a timer. Rollout step 3 in `docs/STORAGE.md` replaces this
//! table with per-subscription windows.

use std::collections::HashMap;
use std::time::Duration;

use popsub_client::Client;
use popsub_config::settings::BrokerSettings;
use popsub_storage::Record;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info, warn};
use tungstenite::protocol::Message as WsMessage;

use crate::frame::{Delivery, TopicFrame};
use crate::topic::{SubscriberId, Topic};

/// A QoS 1 record the broker keeps until it is acknowledged or dropped.
///
/// There is one entry per message id, shared by all subscribers of the topic.
#[derive(Debug, Clone)]
pub struct PendingMessage {
    /// The record as stored, resent unchanged on each retry.
    pub record: Record,
    /// When the record was last sent, in milliseconds since the Unix epoch.
    pub sent_at: i64,
    /// How many times the retry loop has resent the record.
    pub retries: u8,
}

/// The in-memory broker: topics, connected clients and unacknowledged QoS 1 records.
///
/// All methods take `&self` or `&mut self` and never block. The server
/// shares one instance through `SharedBroker`; see the module docs for what
/// that implies.
#[derive(Debug)]
pub struct Broker {
    /// Topics by name. A topic is created on its first subscriber and never removed.
    pub topics: HashMap<String, Topic>,
    /// Connected clients by id, used to reach each subscriber's channel.
    pub clients: HashMap<SubscriberId, Client>,
    /// Unacknowledged QoS 1 records by message id, shared by all subscribers.
    pub pending_acks: HashMap<String, PendingMessage>,
    /// ACK timeout and retry limit used by the retry loop.
    settings: BrokerSettings,
}

impl Broker {
    /// Default ACK timeout of 5000 ms.
    ///
    /// Only tests read this. The retry loop uses `settings.ack_timeout_ms`,
    /// so the two can differ if either default changes.
    pub const ACK_TIMEOUT: Duration = Duration::from_millis(5000);

    /// Creates an empty broker.
    ///
    /// Args:
    ///     settings: ACK timeout and retry limit for QoS 1 records.
    ///
    /// Returns:
    ///     A broker with no topics, clients or pending records.
    pub fn new(settings: BrokerSettings) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
            pending_acks: HashMap::new(),
            settings,
        }
    }

    /// Registers a connected client so records can be delivered to its channel.
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

    /// Returns a copy of a registered client's outgoing channel.
    ///
    /// The topic writer sends replayed records through it without holding
    /// the broker lock.
    ///
    /// Args:
    ///     client_id: The client to look up.
    ///
    /// Returns:
    ///     The channel, or `None` if the client isn't registered.
    pub fn client_sender(&self, client_id: &str) -> Option<UnboundedSender<WsMessage>> {
        self.clients.get(client_id).map(|c| c.sender.clone())
    }

    /// Adds a registered client to a topic's subscribers, creating the topic.
    ///
    /// Replay is not done here; `SharedBroker::subscribe` replays through the
    /// topic writer and then calls this, so no record is missed or sent twice.
    ///
    /// Args:
    ///     topic: The topic name.
    ///     subscriber: The id of the client to add.
    ///
    /// Returns:
    ///     False, without changing anything, when the client isn't
    ///     registered (it disconnected during replay).
    pub fn add_subscriber(&mut self, topic: &str, subscriber: SubscriberId) -> bool {
        if !self.clients.contains_key(&subscriber) {
            return false;
        }
        self.topics
            .entry(topic.to_string())
            .or_insert_with(|| Topic::new(topic))
            .subscribe(subscriber);
        true
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

    /// Sends a newly stored record to every current subscriber of its topic.
    ///
    /// Only the topic's writer calls this, once per record and in `seq`
    /// order, so subscribers see each topic in order. A QoS 1 record is added
    /// to `pending_acks` even when the topic has no subscribers.
    ///
    /// Args:
    ///     record: The record as the log stored it.
    ///     now_ms: The current time, recorded as the send time for retries.
    ///
    /// Returns:
    ///     How many subscribers the record was queued for.
    pub fn deliver(&mut self, record: &Record, now_ms: i64) -> usize {
        if record.qos == 1 {
            self.pending_acks.insert(
                record.message_id.clone(),
                PendingMessage {
                    record: record.clone(),
                    sent_at: now_ms,
                    retries: 0,
                },
            );
        }
        let Some(frame) = TopicFrame::Message(Delivery::from(record)).to_ws() else {
            return 0;
        };
        match self.topics.get(&record.topic) {
            Some(topic) => self.send_to_subscribers(&frame, topic),
            None => 0,
        }
    }

    /// Sends a WebSocket frame to every subscriber of a topic.
    ///
    /// A send to a closed channel is logged, and a subscriber id with no
    /// registered client is logged as a warning. Neither removes the
    /// subscriber; that happens in `cleanup_client`.
    ///
    /// Args:
    ///     frame: The frame to send. It is cloned for each subscriber.
    ///     topic: The topic whose subscribers receive the frame.
    ///
    /// Returns:
    ///     How many subscribers the frame was queued for.
    fn send_to_subscribers(&self, frame: &WsMessage, topic: &Topic) -> usize {
        let mut sent = 0;
        for sub_id in &topic.subscribers {
            match self.clients.get(sub_id) {
                Some(client) => match client.sender.send(frame.clone()) {
                    Ok(()) => sent += 1,
                    Err(e) => error!(client = %sub_id, %e, "failed to send to subscriber"),
                },
                None => warn!(client = %sub_id, "no client registered with id"),
            }
        }
        sent
    }

    /// Finds pending records whose ACK timeout has passed.
    ///
    /// A record is due once more than `settings.ack_timeout_ms` has passed
    /// since it was last sent. Due records that have already been resent
    /// `settings.max_retries` times are marked for dropping; the rest are
    /// marked for resending. Nothing is changed.
    ///
    /// Args:
    ///     now_ms: The current time in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     A pair `(to_resend, to_drop)` of message ids.
    pub(crate) fn collect_retry_candidates(&self, now_ms: i64) -> (Vec<String>, Vec<String>) {
        let mut to_resend = Vec::new();
        let mut to_drop = Vec::new();
        for (message_id, pending) in &self.pending_acks {
            if now_ms - pending.sent_at > self.settings.ack_timeout_ms as i64 {
                if pending.retries >= self.settings.max_retries {
                    to_drop.push(message_id.clone());
                } else {
                    to_resend.push(message_id.clone());
                }
            }
        }
        (to_resend, to_drop)
    }

    /// Resends a pending record to every current subscriber of its topic.
    ///
    /// The entry's `sent_at` is set to `now_ms` and its retry count is
    /// incremented before sending. Subscribers that joined after the original
    /// publish receive the retry too, with the original `seq`.
    ///
    /// Args:
    ///     message_id: The id of the pending record.
    ///     now_ms: The current time in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     `true` if the record was pending, `false` if it was not found.
    pub(crate) fn resend_pending_message(&mut self, message_id: &str, now_ms: i64) -> bool {
        let Some(pending) = self.pending_acks.get_mut(message_id) else {
            return false;
        };
        pending.sent_at = now_ms;
        pending.retries += 1;
        let record = pending.record.clone();
        if let Some(topic) = self.topics.get(&record.topic)
            && let Some(frame) = TopicFrame::Message(Delivery::from(&record)).to_ws()
        {
            self.send_to_subscribers(&frame, topic);
        }
        true
    }

    /// Removes a QoS 1 record from `pending_acks` when a client acknowledges it.
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
            warn!(message_id = %message_id, "received ACK for unknown message_id");
        }
    }

    /// Removes a disconnected client and unsubscribes it from every topic.
    ///
    /// Pending QoS 1 records are not touched, and topics left with no
    /// subscribers are kept.
    ///
    /// Args:
    ///     client_id: The id of the client to remove.
    pub fn cleanup_client(&mut self, client_id: &SubscriberId) {
        self.remove_client(client_id);
        for topic in self.topics.values_mut() {
            topic.unsubscribe(client_id);
        }
        info!(client = %client_id, "cleaned up client");
    }

    /// Resends overdue QoS 1 records and drops those out of retries, in one pass.
    ///
    /// A record is overdue when more than `ack_timeout_ms` has passed since
    /// it was last sent. Overdue records that have already been resent
    /// `max_retries` times are dropped, so each record is sent at most
    /// `1 + max_retries` times. `SharedBroker::run_retry_loop` calls this on a
    /// timer; tests call it directly with a chosen time.
    ///
    /// Args:
    ///     now_ms: The current time in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     How many records were resent and how many were dropped.
    pub fn retry_overdue(&mut self, now_ms: i64) -> RetryOutcome {
        let (to_resend, to_drop) = self.collect_retry_candidates(now_ms);
        for message_id in &to_drop {
            if self.pending_acks.remove(message_id).is_some() {
                warn!(message_id = %message_id, retries = %self.settings.max_retries, "message dropped after max retries");
            }
        }
        for message_id in &to_resend {
            self.resend_pending_message(message_id, now_ms);
        }
        RetryOutcome {
            resent: to_resend.len(),
            dropped: to_drop.len(),
        }
    }
}

/// What one `Broker::retry_overdue` pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryOutcome {
    /// Records sent again to their topic's subscribers.
    pub resent: usize,
    /// Records removed after running out of retries.
    pub dropped: usize,
}
