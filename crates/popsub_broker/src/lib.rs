//! popsub_broker
//!
//! The broker crate contains the central message broker implementation
//! responsible for managing topics, subscriptions, message persistence,
//! and message delivery (including QoS 1 retry/ack behavior).
//!
//! Public types:
//! - `Broker`: core engine to register clients, publish messages, manage topics.
//!
//! This crate is intended to be used by higher-level transport crates (WebSocket server)
//! or binary crates that wire together network, configuration and persistence.

pub mod engine;
pub mod message;
pub mod topic;

pub use engine::Broker;

#[cfg(test)]
mod tests;
