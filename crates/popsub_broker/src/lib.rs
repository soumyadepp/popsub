//! In-memory message broker for PopSub: topics, subscriptions, delivery and QoS 1 retries.
//!
//! The main type is `Broker`, which the WebSocket server reaches through one
//! `SharedBroker` handle. Transport and binary crates register clients with it,
//! forward subscribe, publish and ACK frames to it, and spawn its retry loop.
//! Published messages are also written to `popsub_persistence` so new
//! subscribers can be sent the topic history.

pub mod engine;
pub mod message;
pub mod shared;
pub mod topic;

pub use engine::{Broker, RetryOutcome};
pub use shared::SharedBroker;

#[cfg(test)]
mod tests;
