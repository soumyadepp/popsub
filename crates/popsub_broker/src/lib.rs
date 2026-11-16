//! Message broker for PopSub: topic logs, subscriptions, delivery, replay,
//! per-subscriber acknowledgement and durable sessions.
//!
//! Every published message is appended to its topic's log in a
//! `popsub_storage::Storage` backend, which numbers it with a `seq`, and is
//! then delivered to current subscribers as a tagged `message` frame. A
//! subscriber can ask for history with a `popsub_protocol::StartFrom`; replay and live
//! delivery are handled by one writer task per topic, so they never overlap
//! or leave gaps. Each subscription has its own window of unacknowledged
//! QoS 1 records, and a durable session's cursor is stored so it resumes
//! after a reconnect or a restart. The WebSocket server reaches all of this
//! through one `SharedBroker` handle.

pub mod engine;
mod frame;
mod replay;
pub mod shared;
pub mod topic;
mod writer;

pub use engine::{AckOutcome, Broker, RetryOutcome};
pub use frame::close_message;
pub use shared::{LogSettings, SharedBroker, session_key};
pub use topic::CursorUpdate;
pub use writer::{Published, SubscribeOutcome};

#[cfg(test)]
mod tests;
