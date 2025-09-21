//! The `broker` module is the core of the Pub/Sub system.
//!
//! It contains the following main components:
//!
//! - `Broker`: The central engine that manages topics, clients, and message persistence.
//! - `Topic`: Represents a topic that clients can subscribe to.
//! - `Message`: Represents a message that is published to a topic.
//!
//! The broker is responsible for routing messages from publishers to subscribers.

pub mod engine;
pub mod message;
pub mod topic;

pub use engine::Broker;

#[cfg(test)]
mod tests;
