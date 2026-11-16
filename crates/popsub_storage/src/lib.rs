//! Storage for PopSub: per-topic message logs, durable subscription cursors,
//! and the traits that let operators choose or write their own backend.
//!
//! Every topic is an append-only log of `Record`s numbered by `seq`. A
//! durable subscriber's position is a `Subscription` cursor into that log.
//! Replay reads the recent end of the log, durability comes from the cursor
//! plus the `Durability` level an append is stored at, and `Retention` trims
//! logs from the front. See `docs/STORAGE.md` for the full design.
//!
//! `MemoryStorage` is the built-in backend for now. Any type that implements
//! `Storage` can be used instead, and must pass the suite in `conformance`.

mod error;
mod memory;
mod record;
mod storage;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;

#[cfg(test)]
mod tests;

pub use error::{Result, StorageError};
pub use memory::MemoryStorage;
pub use record::{LogBounds, NewRecord, Record, Subscription};
pub use storage::{Capabilities, Durability, MessageLog, Retention, Storage, SubscriptionStore};
