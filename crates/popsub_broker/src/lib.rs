//! Message broker for PopSub: topic logs, subscriptions, delivery, replay
//! and QoS 1 retries.
//!
//! Every published message is appended to its topic's log in a
//! `popsub_storage::Storage` backend, which numbers it with a `seq`, and is
//! then delivered to current subscribers as a tagged `message` frame. A
//! subscriber can ask for history with a `StartFrom`; replay and live
//! delivery are handled by one writer task per topic, so they never overlap
//! or leave gaps. The WebSocket server reaches all of this through one
//! `SharedBroker` handle.

pub mod engine;
pub mod frame;
mod replay;
pub mod shared;
pub mod topic;
mod writer;

pub use engine::{Broker, RetryOutcome};
pub use frame::{Delivery, StartFrom, TopicFrame, Truncated};
pub use shared::{LogSettings, SharedBroker};
pub use writer::{Published, Subscribed};

#[cfg(test)]
mod tests;
