//! The server's record of one WebSocket connection.
//!
//! Despite the crate name this is not a client library. Transport creates a
//! `Client` for every accepted socket and the broker uses it to reach that
//! socket when routing messages, through the connection's `Outbox`, a queue
//! with a byte budget.

mod outbox;
pub mod pubsub_client;

pub use outbox::{Outbox, OutboxReceiver, Outgoing, QueueLimits, SendError};
pub use pubsub_client::Client;

#[cfg(test)]
mod tests;
