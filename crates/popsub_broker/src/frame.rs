//! The frames the broker writes to subscribers' channels, and the `from`
//! options a subscriber can give for replay.
//!
//! The broker serialises these itself, so it never depends on the transport.
//! The transport's `ServerMessage` wraps `Delivery`, `Truncated`,
//! `Subscribed` and `Stalled` in variants with the same `type` tags, so
//! clients parse every server frame through one enum.

use popsub_storage::Record;
use serde::{Deserialize, Serialize};
use tracing::error;
use tungstenite::protocol::Message as WsMessage;

/// A record delivered to a subscriber, live or replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    /// The topic the record was published to.
    pub topic: String,
    /// The record's position in the topic log. Consumers deduplicate by
    /// `(topic, seq)`.
    pub seq: u64,
    /// The publisher's id for the message.
    pub message_id: String,
    /// The message body.
    pub payload: String,
    /// The delivery level the publisher asked for: 0 or 1.
    pub qos: u8,
    /// When the message was published, in milliseconds since the Unix epoch.
    pub published_at_ms: i64,
}

impl From<&Record> for Delivery {
    /// Builds the frame a subscriber receives for a stored record. The
    /// publisher's username is left out.
    ///
    /// Args:
    ///     record: The stored record.
    ///
    /// Returns:
    ///     The delivery frame.
    fn from(record: &Record) -> Self {
        Self {
            topic: record.topic.clone(),
            seq: record.seq,
            message_id: record.message_id.clone(),
            payload: record.payload.clone(),
            qos: record.qos,
            published_at_ms: record.published_at_ms,
        }
    }
}

/// Tells a subscriber that records it asked to replay were already trimmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Truncated {
    /// The topic subscribed to.
    pub topic: String,
    /// The oldest `seq` still available. Replay starts here.
    pub first_available: u64,
}

/// Confirms a subscription and says which records are history.
///
/// Sent before any replayed record. Records `replay_from` to `replay_to`
/// are history; anything after `replay_to` is live. The range is empty when
/// `replay_from` is greater than `replay_to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribed {
    /// The topic subscribed to.
    pub topic: String,
    /// The first `seq` that will be sent.
    pub replay_from: u64,
    /// The newest `seq` in the log when the subscription started.
    pub replay_to: u64,
}

/// Tells a subscriber that a QoS 1 record ran out of retries.
///
/// Delivery to that subscription pauses until the client acknowledges a
/// record or subscribes again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stalled {
    /// The subscribed topic.
    pub topic: String,
    /// The record that was never acknowledged.
    pub seq: u64,
}

/// Every frame the broker sends, tagged by `type` like the transport's frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TopicFrame {
    /// A delivered record, `{"type":"message", ...}`.
    Message(Delivery),
    /// Records the subscriber expected were trimmed, `{"type":"truncated", ...}`.
    Truncated(Truncated),
    /// A subscription started, `{"type":"subscribed", ...}`.
    Subscribed(Subscribed),
    /// A subscription paused after a record ran out of retries,
    /// `{"type":"stalled", ...}`.
    Stalled(Stalled),
}

impl TopicFrame {
    /// Serialises the frame as a WebSocket text message.
    ///
    /// Returns:
    ///     The message, or `None` (after logging) if serialisation fails,
    ///     which only happens if the frame types are changed incorrectly.
    pub fn to_ws(&self) -> Option<WsMessage> {
        match serde_json::to_string(self) {
            Ok(json) => Some(WsMessage::text(json)),
            Err(e) => {
                error!(%e, "failed to serialise a topic frame");
                None
            }
        }
    }
}

/// Where replay starts when a client subscribes.
///
/// On the wire this is `"resume"`, `"new"`, `{"last": 50}`,
/// `{"since_ms": 1794800000000}` or `{"seq": 1200}`. A subscribe without
/// `from` means `"resume"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartFrom {
    /// One past the stored cursor of this session's subscription to the
    /// topic. For an ephemeral connection, or a session with no stored
    /// subscription to the topic, the same as `New`.
    #[default]
    Resume,
    /// No replay. Only records published after subscribing.
    New,
    /// The last N retained records.
    Last(u64),
    /// The first record published at or after this time, in milliseconds
    /// since the Unix epoch.
    SinceMs(i64),
    /// This `seq`, or the oldest retained one if it was trimmed.
    Seq(u64),
}
