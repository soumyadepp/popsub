//! The `Message` type the broker routes, stores and sends to clients.
//!
//! The same struct is the internal form and, serialised as JSON, the frame
//! subscribers receive.

use serde::{Deserialize, Serialize};

/// A message published to a topic.
///
/// `qos` 0 is at-most-once delivery. `qos` 1 makes the broker keep the
/// message in its pending table and resend it until it is acknowledged or
/// runs out of retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Name of the topic the message is routed to.
    pub topic: String,
    /// Message body as a string, sent to subscribers unchanged.
    pub payload: String,
    /// Milliseconds since the UNIX epoch. The broker overwrites it on publish.
    pub timestamp: i64,
    /// Id clients send back in an ACK. The broker generates a UUID when it is empty.
    pub message_id: String,
    /// Delivery mode: `0` for at-most-once, `1` for at-least-once with retries.
    pub qos: u8,
}
