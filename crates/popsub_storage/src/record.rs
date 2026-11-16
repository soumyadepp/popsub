//! The records a topic log stores, the subscriptions that point into it, and
//! the bounds of a log.

use serde::{Deserialize, Serialize};

/// A message to append to a topic log, before the log gives it a sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRecord {
    /// The publisher's idempotency key. Appending the same id again from the
    /// same publisher to the same topic returns the original `seq`.
    pub message_id: String,
    /// Username of the client that published the message.
    pub publisher: String,
    /// The message body.
    pub payload: String,
    /// Delivery level requested by the publisher: 0 or 1.
    pub qos: u8,
    /// When the message was published, in milliseconds since the Unix epoch.
    pub published_at_ms: i64,
}

/// A record as stored in a topic log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The topic whose log holds the record.
    pub topic: String,
    /// Position in the topic log. Starts at 1, increases by exactly 1 and is
    /// never reused, even after the record is trimmed.
    pub seq: u64,
    /// The publisher's idempotency key.
    pub message_id: String,
    /// Username of the client that published the message.
    pub publisher: String,
    /// The message body.
    pub payload: String,
    /// Delivery level requested by the publisher: 0 or 1.
    pub qos: u8,
    /// When the message was published, in milliseconds since the Unix epoch.
    /// Never lower than the previous record's, because the log raises it to
    /// that value if the publisher's clock was behind.
    pub published_at_ms: i64,
}

impl Record {
    /// Builds the stored form of a new record once the log has chosen its place.
    ///
    /// Args:
    ///     topic: The topic whose log holds the record.
    ///     seq: The sequence number the log assigned.
    ///     new: The record as the publisher sent it.
    ///     published_at_ms: The publish time the log settled on, which may be
    ///         later than `new.published_at_ms`.
    ///
    /// Returns:
    ///     The record to store.
    pub fn from_new(topic: &str, seq: u64, new: NewRecord, published_at_ms: i64) -> Self {
        Self {
            topic: topic.to_string(),
            seq,
            message_id: new.message_id,
            publisher: new.publisher,
            payload: new.payload,
            qos: new.qos,
            published_at_ms,
        }
    }

    /// The bytes this record counts against `Retention::max_bytes`.
    ///
    /// Only the variable-length fields are counted (message id, publisher and
    /// payload), so the figure is the same whatever encoding a backend uses.
    ///
    /// Returns:
    ///     The size in bytes.
    pub fn size(&self) -> u64 {
        (self.message_id.len() + self.publisher.len() + self.payload.len()) as u64
    }
}

/// A session's subscription to a topic and how far it has acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The session that holds the subscription.
    pub session_id: String,
    /// The subscribed topic.
    pub topic: String,
    /// The highest `seq` acknowledged without gaps. Only moves forward. Zero
    /// means nothing has been acknowledged.
    pub acked_seq: u64,
}

/// The range of sequence numbers a topic log holds.
///
/// A log that has never had a record has bounds `1..=0`. A log whose records
/// were all trimmed keeps its `last_seq`, so its bounds are
/// `last_seq + 1..=last_seq` and new records continue the sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogBounds {
    /// The oldest retained `seq`, or `last_seq + 1` when nothing is retained.
    pub first_seq: u64,
    /// The newest `seq` ever appended, or 0 when nothing ever was.
    pub last_seq: u64,
}

impl LogBounds {
    /// The bounds of a topic that has never had a record.
    pub const EMPTY: LogBounds = LogBounds {
        first_seq: 1,
        last_seq: 0,
    };

    /// Whether the log currently retains no records.
    ///
    /// Returns:
    ///     True when there is nothing to read.
    pub fn is_empty(&self) -> bool {
        self.first_seq > self.last_seq
    }

    /// The number of records the log retains.
    ///
    /// Returns:
    ///     The count, zero when the log is empty.
    pub fn len(&self) -> u64 {
        (self.last_seq + 1).saturating_sub(self.first_seq)
    }

    /// The `seq` the next appended record will get.
    ///
    /// Returns:
    ///     One past `last_seq`.
    pub fn next_seq(&self) -> u64 {
        self.last_seq + 1
    }
}
