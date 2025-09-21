//! The `client` module defines the representation of a client in the Pub/Sub system.
//!
//! It provides the `Client` struct, which encapsulates the state of a single
//! connected client, including its unique identifier and the channel for sending
//! messages to it.

pub mod pubsub_client;
pub use pubsub_client::Client;

#[cfg(test)]
mod tests;
