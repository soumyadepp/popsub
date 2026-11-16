//! The `Broker` engine: connected clients, topic subscriptions and each
//! subscription's in-flight window, acknowledgements and retries.
//!
//! The broker is synchronous, has no locking of its own and does no I/O
//! beyond pushing `tungstenite` messages into unbounded channels, so nothing
//! here waits. Storage is not reached from here: each topic's writer (see
//! `writer`) reads and appends to the log without the lock, then calls into
//! the broker under it. The server shares the broker through
//! `SharedBroker`, whose `with` runs a closure under the lock.
//!
//! Every subscription has its own delivery state (see `topic`). A record is
//! sent to a subscription only when it is the next `seq` that subscription
//! expects and its window has room; otherwise the subscription has fallen
//! behind and the writer catches it up from the log once acks free the
//! window. QoS 1 records stay in the subscription's window until that
//! subscription acknowledges them, so an ACK from one subscriber never
//! clears a record for another. A record that runs out of retries stalls
//! its subscription instead of being dropped silently.
//!
//! Methods that move a durable session's cursor return `CursorUpdate`s for
//! the caller to write to the subscription store outside the lock.

use std::collections::HashMap;
use std::time::Duration;

use popsub_client::{Client, Outbox};
use popsub_config::settings::BrokerSettings;
use popsub_storage::Record;
use tracing::{debug, info, warn};

use popsub_protocol::{ServerFrame, ServerMessage, Stalled};

use crate::frame::{delivery, to_ws};
use crate::topic::{CursorUpdate, DeliveryState, InFlight, SubscriberId, Topic};

/// The broker: connected clients and topic subscriptions with their delivery state.
///
/// All methods take `&self` or `&mut self` and never block. The server
/// shares one instance through `SharedBroker`; see the module docs.
#[derive(Debug)]
pub struct Broker {
    /// Topics by name. A topic is created on its first subscriber and never removed.
    pub topics: HashMap<String, Topic>,
    /// Connected clients by id, used to reach each subscriber's channel.
    pub clients: HashMap<SubscriberId, Client>,
    /// ACK timeout, retry limit and window size.
    settings: BrokerSettings,
}

/// What an `ack` did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AckOutcome {
    /// Whether the record was in flight to this client on this topic.
    /// Anything else (a record sent to someone else, a repeat ACK, an
    /// unknown topic) is ignored.
    pub accepted: bool,
    /// The cursor to store, when the ACK moved a durable session's cursor.
    pub cursor: Option<CursorUpdate>,
    /// Whether the subscription has room again and records waiting in the
    /// log, so the topic's writer should catch it up.
    pub catch_up: bool,
}

/// What `Broker::deliver` did, beyond queueing frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliverOutcome {
    /// Cursors that moved, to be stored.
    pub cursors: Vec<CursorUpdate>,
    /// Clients skipped because their outgoing queue was over its soft
    /// limit, that nobody is yet waiting on. The caller arranges a
    /// catch-up for each once its queue drains.
    pub backed_up: Vec<SubscriberId>,
}

/// What happened when one record was offered to one subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    /// It was queued.
    Sent,
    /// The subscription expects another `seq` (it is behind or ahead).
    NotNext,
    /// The subscription's window is full or it is stalled.
    NoWindow,
    /// The client's outgoing queue is over its soft limit.
    Busy,
    /// The client's connection is closing.
    Closed,
}

/// What one `Broker::retry_overdue` pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryOutcome {
    /// Records sent again to the one subscription that hadn't acknowledged them.
    pub resent: usize,
    /// Subscriptions that ran out of retries and were paused.
    pub stalled: usize,
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
    ///     settings: ACK timeout, retry limit and window size.
    ///
    /// Returns:
    ///     A broker with no topics or clients.
    pub fn new(settings: BrokerSettings) -> Self {
        Self {
            topics: HashMap::new(),
            clients: HashMap::new(),
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

    /// Returns a handle on a registered client's outgoing queue.
    ///
    /// Args:
    ///     client_id: The client to look up.
    ///
    /// Returns:
    ///     The queue, or `None` if the client isn't registered.
    pub fn client_sender(&self, client_id: &str) -> Option<Outbox> {
        self.clients.get(client_id).map(|c| c.sender.clone())
    }

    /// The window size subscriptions get, as configured.
    ///
    /// Returns:
    ///     The most unacknowledged QoS 1 records per subscription, at least 1.
    pub fn max_inflight(&self) -> usize {
        self.settings.max_inflight.max(1)
    }

    /// Returns the durable session a registered client holds.
    ///
    /// Args:
    ///     client_id: The client to look up.
    ///
    /// Returns:
    ///     The session's storage key, or `None` for an ephemeral or unknown client.
    pub fn client_session(&self, client_id: &str) -> Option<String> {
        self.clients.get(client_id)?.session.clone()
    }

    /// Finds the connected client holding a durable session.
    ///
    /// Args:
    ///     session: The session's storage key.
    ///
    /// Returns:
    ///     The client's id, or `None` when no connection holds it.
    pub fn session_holder(&self, session: &str) -> Option<SubscriberId> {
        self.clients
            .values()
            .find(|c| c.session.as_deref() == Some(session))
            .map(|c| c.id.clone())
    }

    /// Lists the storage keys of every connected durable session.
    ///
    /// Returns:
    ///     The keys, in no particular order.
    pub fn connected_sessions(&self) -> Vec<String> {
        self.clients
            .values()
            .filter_map(|c| c.session.clone())
            .collect()
    }

    /// Queues a frame for one client.
    ///
    /// Args:
    ///     client_id: The client.
    ///     frame: The frame to send.
    ///
    /// Returns:
    ///     False when the client isn't registered or its channel is closed.
    pub fn send_frame(&self, client_id: &str, frame: &ServerFrame) -> bool {
        let (Some(client), Some(ws)) = (self.clients.get(client_id), to_ws(frame)) else {
            return false;
        };
        client.sender.send(ws).is_ok()
    }

    /// Adds a subscription for a registered client, creating the topic.
    ///
    /// Replaces any subscription the client already had to the topic, which
    /// discards its in-flight records. Nothing is sent here; the topic's
    /// writer replays from `start_seq` afterwards.
    ///
    /// Args:
    ///     topic: The topic name.
    ///     subscriber: The id of the client.
    ///     start_seq: The first `seq` to send it.
    ///     log_last_seq: The newest `seq` in the topic log right now.
    ///
    /// Returns:
    ///     False, without changing anything, when the client isn't registered.
    pub fn add_subscriber(
        &mut self,
        topic: &str,
        subscriber: SubscriberId,
        start_seq: u64,
        log_last_seq: u64,
    ) -> bool {
        let Some(client) = self.clients.get(&subscriber) else {
            return false;
        };
        let state = DeliveryState::new(start_seq, client.session.clone());
        let topic = self
            .topics
            .entry(topic.to_string())
            .or_insert_with(|| Topic::new(topic));
        topic.last_seq = topic.last_seq.max(log_last_seq);
        topic.subscribe(subscriber, state);
        true
    }

    /// Removes a client's subscription to a topic.
    ///
    /// Does nothing if the topic or subscription doesn't exist. The topic
    /// itself is kept even when it has no subscribers left.
    ///
    /// Args:
    ///     topic: The topic name.
    ///     subscriber: The id of the client to remove.
    ///
    /// Returns:
    ///     The session whose stored subscription should also be removed, if
    ///     the subscription belonged to a durable session.
    pub fn unsubscribe(&mut self, topic: &str, subscriber: &SubscriberId) -> Option<String> {
        self.topics.get_mut(topic)?.unsubscribe(subscriber)?.session
    }

    /// Sends a newly stored record to every subscription that is up to date.
    ///
    /// Only the topic's writer calls this, once per record and in `seq`
    /// order. Subscriptions that are behind, stalled, out of window room, or
    /// whose client's outgoing queue is over its soft limit are skipped; the
    /// writer catches them up from the log later.
    ///
    /// Args:
    ///     record: The record as the log stored it.
    ///     now_ms: The current time, recorded as the send time for retries.
    ///
    /// Returns:
    ///     The cursors that moved, and the clients newly skipped for a full
    ///     queue, which need a catch-up once it drains.
    pub fn deliver(&mut self, record: &Record, now_ms: i64) -> DeliverOutcome {
        let max_inflight = self.settings.max_inflight;
        let Some(topic) = self.topics.get_mut(&record.topic) else {
            return DeliverOutcome::default();
        };
        topic.last_seq = topic.last_seq.max(record.seq);
        let mut outcome = DeliverOutcome::default();
        for (client_id, state) in topic.subscribers.iter_mut() {
            let Some(client) = self.clients.get(client_id) else {
                continue;
            };
            match send_one(state, &client.sender, record, now_ms, max_inflight) {
                Offer::Sent => outcome.cursors.extend(state.take_update(&topic.name)),
                Offer::Busy if !state.awaiting_room => {
                    state.awaiting_room = true;
                    outcome.backed_up.push(client_id.clone());
                }
                _ => {}
            }
        }
        outcome
    }

    /// Notes that a subscription is being caught up, so the next time its
    /// client's queue fills a new catch-up is arranged.
    ///
    /// Args:
    ///     topic: The topic.
    ///     client_id: The subscribed client.
    pub fn room_restored(&mut self, topic: &str, client_id: &str) {
        if let Some(state) = self
            .topics
            .get_mut(topic)
            .and_then(|t| t.subscribers.get_mut(client_id))
        {
            state.awaiting_room = false;
        }
    }

    /// Reports where a subscription that has room wants records from.
    ///
    /// Args:
    ///     topic: The topic.
    ///     client_id: The subscribed client.
    ///
    /// Returns:
    ///     Its `next_seq`, or `None` when it doesn't exist, is stalled, has a
    ///     full window, or its client's outgoing queue is over the soft limit.
    pub fn wants(&self, topic: &str, client_id: &str) -> Option<u64> {
        let state = self.topics.get(topic)?.subscribers.get(client_id)?;
        let client = self.clients.get(client_id)?;
        (state.has_room(self.settings.max_inflight) && client.sender.has_room())
            .then_some(state.next_seq)
    }

    /// Sends records read from the log to one subscription, in order.
    ///
    /// Stops at the first record that isn't the subscription's `next_seq`,
    /// or when its window fills, its client's queue goes over the soft
    /// limit, or its connection is closing.
    ///
    /// Args:
    ///     topic: The topic.
    ///     client_id: The subscribed client.
    ///     records: Consecutive records from the log.
    ///     now_ms: The current time, recorded as the send time for retries.
    ///
    /// Returns:
    ///     How many records were sent, the cursor to store if it moved, and
    ///     whether it stopped for a full queue with nobody yet waiting on it
    ///     (the caller then arranges a catch-up once the queue drains).
    pub fn send_records(
        &mut self,
        topic: &str,
        client_id: &str,
        records: &[Record],
        now_ms: i64,
    ) -> (usize, Option<CursorUpdate>, bool) {
        let max_inflight = self.settings.max_inflight;
        let (Some(topic), Some(client)) = (self.topics.get_mut(topic), self.clients.get(client_id))
        else {
            return (0, None, false);
        };
        let Some(state) = topic.subscribers.get_mut(client_id) else {
            return (0, None, false);
        };
        let mut sent = 0;
        let mut backed_up = false;
        for record in records {
            match send_one(state, &client.sender, record, now_ms, max_inflight) {
                Offer::Sent => sent += 1,
                Offer::Busy => {
                    backed_up = !state.awaiting_room;
                    state.awaiting_room = true;
                    break;
                }
                _ => break,
            }
        }
        (sent, state.take_update(&topic.name), backed_up)
    }

    /// Moves a subscription past records that were trimmed before it got them.
    ///
    /// Args:
    ///     topic: The topic.
    ///     client_id: The subscribed client.
    ///     seq: The oldest `seq` still in the log.
    ///
    /// Returns:
    ///     The cursor to store if it moved.
    pub fn skip_to(&mut self, topic: &str, client_id: &str, seq: u64) -> Option<CursorUpdate> {
        let topic = self.topics.get_mut(topic)?;
        let state = topic.subscribers.get_mut(client_id)?;
        state.next_seq = state.next_seq.max(seq);
        state.take_update(&topic.name)
    }

    /// Records a client's acknowledgement of one record.
    ///
    /// Only a record in flight to this client on this topic is accepted. An
    /// accepted ACK also lifts a stall and gives the remaining in-flight
    /// records a fresh set of retries.
    ///
    /// Args:
    ///     client_id: The acknowledging client.
    ///     topic: The topic the record was delivered on.
    ///     seq: The acknowledged record.
    ///     now_ms: The current time, used as the new send time when a stall is lifted.
    ///
    /// Returns:
    ///     Whether it was accepted, the cursor to store, and whether the
    ///     writer should catch the subscription up.
    pub fn ack(&mut self, client_id: &str, topic: &str, seq: u64, now_ms: i64) -> AckOutcome {
        let max_inflight = self.settings.max_inflight;
        let Some(topic) = self.topics.get_mut(topic) else {
            return AckOutcome::default();
        };
        let Some(state) = topic.subscribers.get_mut(client_id) else {
            return AckOutcome::default();
        };
        if state.inflight.remove(&seq).is_none() {
            debug!(client = %client_id, topic = %topic.name, seq, "ignored ACK for a record not in flight to this client");
            return AckOutcome::default();
        }
        if state.stalled.take().is_some() {
            for pending in state.inflight.values_mut() {
                pending.retries = 0;
                pending.sent_at = now_ms;
            }
        }
        AckOutcome {
            accepted: true,
            cursor: state.take_update(&topic.name),
            catch_up: state.has_room(max_inflight) && state.next_seq <= topic.last_seq,
        }
    }

    /// Removes a disconnected client and all its subscriptions.
    ///
    /// In-flight records are forgotten. For a durable session they are sent
    /// again when it reconnects, because its stored cursor is below them.
    ///
    /// Args:
    ///     client_id: The id of the client to remove.
    ///
    /// Returns:
    ///     The durable session the client held, if any.
    pub fn cleanup_client(&mut self, client_id: &SubscriberId) -> Option<String> {
        let session = self.clients.remove(client_id).and_then(|c| c.session);
        for topic in self.topics.values_mut() {
            topic.unsubscribe(client_id);
        }
        info!(client = %client_id, "cleaned up client");
        session
    }

    /// Resends overdue QoS 1 records and stalls subscriptions out of retries, in one pass.
    ///
    /// A record is overdue when more than `ack_timeout_ms` has passed since
    /// it was last sent to that subscription. It is resent only to that
    /// subscription. A record already resent `max_retries` times stalls its
    /// subscription instead: the client is sent a `stalled` frame and
    /// nothing more is sent to that subscription until it acknowledges
    /// something or subscribes again. `SharedBroker::run_retry_loop` calls
    /// this on a timer; tests call it directly with a chosen time.
    ///
    /// Args:
    ///     now_ms: The current time in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     How many records were resent and how many subscriptions stalled.
    pub fn retry_overdue(&mut self, now_ms: i64) -> RetryOutcome {
        let timeout = i64::try_from(self.settings.ack_timeout_ms).unwrap_or(i64::MAX);
        let max_retries = self.settings.max_retries;
        let mut outcome = RetryOutcome {
            resent: 0,
            stalled: 0,
        };
        for topic in self.topics.values_mut() {
            for (client_id, state) in topic.subscribers.iter_mut() {
                if state.stalled.is_some() {
                    continue;
                }
                let Some(client) = self.clients.get(client_id) else {
                    continue;
                };
                for (&seq, pending) in state.inflight.iter_mut() {
                    if now_ms.saturating_sub(pending.sent_at) <= timeout {
                        continue;
                    }
                    if pending.retries >= max_retries {
                        state.stalled = Some(seq);
                        outcome.stalled += 1;
                        warn!(client = %client_id, topic = %topic.name, seq, "subscription stalled after max retries");
                        let frame = ServerFrame::event(ServerMessage::Stalled(Stalled {
                            topic: topic.name.clone(),
                            seq,
                        }));
                        if let Some(ws) = to_ws(&frame) {
                            let _ = client.sender.send(ws);
                        }
                        break;
                    }
                    pending.retries += 1;
                    pending.sent_at = now_ms;
                    if let Some(ws) = to_ws(&delivery(&pending.record)) {
                        let _ = client.sender.send(ws);
                    }
                    outcome.resent += 1;
                }
            }
        }
        outcome
    }
}

/// Offers one record to one subscription: sends it if it is the next one
/// the subscription expects, its window has room, and its client's queue is
/// under the soft limit.
///
/// A QoS 1 record goes into the subscription's window; a QoS 0 record is
/// done once sent.
///
/// Args:
///     state: The subscription's delivery state.
///     sender: The client's outgoing queue.
///     record: The record.
///     now_ms: The send time, for retries.
///     max_inflight: The window size.
///
/// Returns:
///     What happened.
fn send_one(
    state: &mut DeliveryState,
    sender: &Outbox,
    record: &Record,
    now_ms: i64,
    max_inflight: usize,
) -> Offer {
    if state.next_seq != record.seq {
        return Offer::NotNext;
    }
    if !state.has_room(max_inflight) {
        return Offer::NoWindow;
    }
    if sender.closing().is_some() {
        return Offer::Closed;
    }
    if !sender.has_room() {
        return Offer::Busy;
    }
    let Some(ws) = to_ws(&delivery(record)) else {
        return Offer::Closed;
    };
    if sender.send(ws).is_err() {
        return Offer::Closed;
    }
    state.next_seq = record.seq + 1;
    if record.qos == 1 {
        state.inflight.insert(
            record.seq,
            InFlight {
                record: record.clone(),
                sent_at: now_ms,
                retries: 0,
            },
        );
    }
    Offer::Sent
}
