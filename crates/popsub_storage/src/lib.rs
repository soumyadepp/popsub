//! Storage for PopSub: per-topic message logs, durable subscription cursors,
//! and the traits that let operators choose or write their own backend.
//!
//! Every topic is an append-only log of `Record`s numbered by `seq`. A
//! durable subscriber's position is a `Subscription` cursor into that log.
//! Replay reads the recent end of the log, durability comes from the cursor
//! plus the `Durability` level an append is stored at, and `Retention` trims
//! logs from the front. See `docs/STORAGE.md` for the full design.
//!
//! Two backends are built in: `FileStorage`, which keeps logs and sessions
//! in a local directory and survives restarts, and `MemoryStorage`, which
//! keeps everything in the process. Any type that implements `Storage` can
//! be used instead, and must pass the suite in `conformance`. `FaultyStorage`
//! wraps any backend and fails its appends on demand, for testing what
//! happens when storage breaks.

mod error;
mod faulty;
mod file;
mod index;
mod memory;
mod record;
mod sessions;
mod storage;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;

#[cfg(test)]
mod tests;

pub use error::{Result, StorageError};
pub use faulty::FaultyStorage;
pub use file::{DEFAULT_SEGMENT_BYTES, FileOptions, FileStorage, MAX_TOPIC_BYTES};
pub use memory::MemoryStorage;
pub use record::{LogBounds, NewRecord, Record, Subscription};
pub use storage::{Capabilities, Durability, MessageLog, Retention, Storage, SubscriptionStore};
