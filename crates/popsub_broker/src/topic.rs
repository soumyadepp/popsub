//! A topic's subscribers and each subscription's delivery state.
//!
//! `Topic` has no locking of its own. The broker owns every topic and
//! callers reach them through the broker's mutex.
//!
//! Each subscription remembers the next `seq` to send it and the QoS 1
//! records sent but not yet acknowledged. Everything below `next_seq` that
//! is not in flight has been delivered and needs nothing more, so the
//! subscription's cursor (the highest `seq` with nothing outstanding at or
//! below it) is derived from those two values rather than stored.

use std::collections::{BTreeMap, HashMap};

use popsub_storage::Record;

/// Id of a subscribed client, the same string as `Client::id`.
pub type SubscriberId = String;

/// A QoS 1 record sent to one subscription and not yet acknowledged.
#[derive(Debug, Clone)]
pub struct InFlight {
    /// The record, resent unchanged on each retry.
    pub record: Record,
    /// When the record was last sent, in milliseconds since the Unix epoch.
    pub sent_at: i64,
    /// How many times the record has been resent to this subscription.
    pub retries: u8,
}

/// A cursor that moved and must be written to the subscription store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorUpdate {
    /// The session's storage key.
    pub session: String,
    /// The subscribed topic.
    pub topic: String,
    /// The new cursor.
    pub acked_seq: u64,
}

/// One subscription's progress through a topic log.
#[derive(Debug, Clone)]
pub struct DeliveryState {
    /// The durable session's storage key, or `None` for an ephemeral
    /// connection, whose cursor is never stored.
    pub session: Option<String>,
    /// The next `seq` to send. Live records are only sent when they are
    /// exactly this one; anything later is read from the log by catch-up.
    pub next_seq: u64,
    /// QoS 1 records sent and not yet acknowledged, by `seq`.
    pub inflight: BTreeMap<u64, InFlight>,
    /// The `seq` that ran out of retries. While set, nothing is sent or
    /// resent to this subscription.
    pub stalled: Option<u64>,
    /// The cursor last handed out as a `CursorUpdate`.
    persisted: u64,
}

impl DeliveryState {
    /// Starts a subscription that will be sent records from `start_seq` on.
    ///
    /// Args:
    ///     start_seq: The first `seq` to send.
    ///     session: The durable session's storage key, if any.
    ///
    /// Returns:
    ///     A state whose cursor is `start_seq - 1`.
    pub fn new(start_seq: u64, session: Option<String>) -> Self {
        let start_seq = start_seq.max(1);
        Self {
            session,
            next_seq: start_seq,
            inflight: BTreeMap::new(),
            stalled: None,
            persisted: start_seq - 1,
        }
    }

    /// The highest `seq` with nothing outstanding at or below it.
    ///
    /// Returns:
    ///     One below the oldest record in flight, or one below `next_seq`
    ///     when nothing is in flight.
    pub fn cursor(&self) -> u64 {
        self.inflight
            .keys()
            .next()
            .map_or(self.next_seq, |&seq| seq)
            .saturating_sub(1)
    }

    /// Reports a cursor that has moved past what was last persisted.
    ///
    /// Ephemeral subscriptions never report one.
    ///
    /// Args:
    ///     topic: The subscribed topic, for the update.
    ///
    /// Returns:
    ///     The update to write, or `None` when nothing needs writing.
    pub fn take_update(&mut self, topic: &str) -> Option<CursorUpdate> {
        let session = self.session.as_ref()?;
        let cursor = self.cursor();
        if cursor <= self.persisted {
            return None;
        }
        self.persisted = cursor;
        Some(CursorUpdate {
            session: session.clone(),
            topic: topic.to_string(),
            acked_seq: cursor,
        })
    }

    /// Whether this subscription may be sent its next record now.
    ///
    /// Args:
    ///     max_inflight: The window size. Below 1 counts as 1.
    ///
    /// Returns:
    ///     False while stalled or while the window is full.
    pub fn has_room(&self, max_inflight: usize) -> bool {
        self.stalled.is_none() && self.inflight.len() < max_inflight.max(1)
    }
}

/// A named topic, its subscriptions, and the newest `seq` the broker has seen.
#[derive(Debug, Default)]
pub struct Topic {
    /// Topic name, the key the broker routes on.
    pub name: String,
    /// Each subscribed client's delivery state.
    pub subscribers: HashMap<SubscriberId, DeliveryState>,
    /// The highest `seq` delivered or known at subscribe time, used to tell
    /// whether a subscription has fallen behind and needs catching up.
    pub last_seq: u64,
}

impl Topic {
    /// Creates a topic with no subscribers.
    ///
    /// Args:
    ///     name: The topic name.
    ///
    /// Returns:
    ///     The new, empty topic.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subscribers: HashMap::new(),
            last_seq: 0,
        }
    }

    /// Adds a subscription, replacing any the client already had.
    ///
    /// Args:
    ///     id: The id of the client.
    ///     state: Where its delivery starts.
    pub fn subscribe(&mut self, id: SubscriberId, state: DeliveryState) {
        self.subscribers.insert(id, state);
    }

    /// Removes a client's subscription. Removing an unknown id does nothing.
    ///
    /// Args:
    ///     id: The id of the client to remove.
    ///
    /// Returns:
    ///     The removed state, if there was one.
    pub fn unsubscribe(&mut self, id: &SubscriberId) -> Option<DeliveryState> {
        self.subscribers.remove(id)
    }
}
