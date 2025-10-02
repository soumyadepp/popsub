//! Message definitions for the broker
//!
//! `Message` is the canonical wire/internal representation used by the
//! broker. Fields are chosen to support both QoS=0 (best-effort) and QoS=1
//! (at-least-once) delivery semantics.
//!
//! Notes on fields:
//! - `topic`: topic name used for routing
//! - `payload`: JSON-serializable body as a String (the protocol is JSON)
//! - `timestamp`: milliseconds since UNIX epoch; set by the broker upon publish
//! - `message_id`: opaque unique id used for QoS=1 ACKs; the broker will
//!   generate one if the client does not provide it
//! - `qos`: delivery mode. `0` = at-most-once, `1` = at-least-once

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub topic: String,
    pub payload: String,
    pub timestamp: i64,
    pub message_id: String,
    pub qos: u8,
}
