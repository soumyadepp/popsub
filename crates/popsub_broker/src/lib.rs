//! In-memory message broker for PopSub: topics, subscriptions, delivery and QoS 1 retries.
//!
//! The main type is `Broker`, which the WebSocket server keeps behind one
//! shared mutex. Transport and binary crates register clients with it,
//! forward subscribe, publish and ACK frames to it, and spawn its retry loop.
//! Published messages are also written to `popsub_persistence` so new
//! subscribers can be sent the topic history.

pub mod engine;
pub mod message;
pub mod topic;

pub use engine::Broker;

#[cfg(test)]
mod tests;
