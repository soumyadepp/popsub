//! The `persistence` module provides mechanisms for storing and retrieving messages.
//!
//! This is crucial for features like message replay, where clients that
//! reconnect or subscribe to a topic can receive a history of past messages.
//!
//! Currently, it uses `sled` as an embedded key-value store for efficient
//! and durable message storage.

pub mod sled_store;
