//! The storage traits every backend implements, and the durability and
//! retention policies they are given.
//!
//! `MessageLog` holds the per-topic logs and `SubscriptionStore` holds session
//! cursors. They are separate because their access patterns differ
//! (sequential append and scan versus small random updates), but most
//! backends implement both on one type and then `Storage`, which adds what
//! the backend can promise.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{Result, StorageError};
use crate::record::{LogBounds, NewRecord, Record, Subscription};

/// How firmly an append must be stored before it counts as done.
///
/// Levels are ordered, so a backend that supports `Synced` also supports the
/// two below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    /// Held in memory only. Lost if the process stops.
    None,
    /// Handed to the operating system. Survives the process crashing, not the
    /// machine losing power.
    Buffered,
    /// Flushed to stable storage, or committed by a database.
    Synced,
}

/// What a backend can promise, checked against the configuration at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// The strongest durability level the backend can honour.
    pub max_durability: Durability,
    /// Whether records and cursors survive the backend being reopened.
    pub persistent: bool,
}

impl Capabilities {
    /// Checks that the backend can honour a durability level.
    ///
    /// Args:
    ///     durability: The level an append or the configuration asks for.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When `durability` is stronger than
    ///         `max_durability`.
    pub fn check(&self, durability: Durability) -> Result<()> {
        if durability > self.max_durability {
            return Err(StorageError::Unsupported(format!(
                "durability {durability:?} requested, backend provides at most {:?}",
                self.max_durability
            )));
        }
        Ok(())
    }
}

/// What a backup copied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackupSummary {
    /// Topics in the backup, including ones whose records were all trimmed.
    pub topics: u64,
    /// Retained records across every topic.
    pub records: u64,
    /// Bytes written, counting segments, heads and the session log.
    pub bytes: u64,
}

/// Which records a topic log keeps. Records outside any limit are trimmed
/// from the front. A limit left as `None` doesn't apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retention {
    /// Keep at most this many records.
    pub max_messages: Option<u64>,
    /// Keep records published at most this many milliseconds ago.
    pub max_age_ms: Option<u64>,
    /// Keep at most this many bytes, as counted by `Record::size`.
    pub max_bytes: Option<u64>,
}

impl Retention {
    /// Decides whether a log's oldest record must be trimmed.
    ///
    /// Backends call this in a loop, removing the oldest record each time it
    /// returns true, so every backend trims by the same rules.
    ///
    /// Args:
    ///     count: How many records the log holds now.
    ///     bytes: The total `Record::size` of those records.
    ///     oldest_published_at_ms: When the oldest record was published.
    ///     now_ms: The current time, in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     True when the oldest record is outside at least one limit.
    pub fn must_trim(
        &self,
        count: u64,
        bytes: u64,
        oldest_published_at_ms: i64,
        now_ms: i64,
    ) -> bool {
        if count == 0 {
            return false;
        }
        let over_count = self.max_messages.is_some_and(|max| count > max);
        let over_bytes = self.max_bytes.is_some_and(|max| bytes > max);
        let too_old = self.max_age_ms.is_some_and(|max| {
            now_ms.saturating_sub(oldest_published_at_ms) > i64::try_from(max).unwrap_or(i64::MAX)
        });
        over_count || over_bytes || too_old
    }
}

/// Append-only, per-topic logs of numbered records.
#[async_trait]
pub trait MessageLog: Send + Sync {
    /// Appends a record and returns it as stored.
    ///
    /// If the same `(topic, publisher, message_id)` is still retained, nothing
    /// is appended and the original record is returned, so a publisher
    /// retrying after a lost confirmation doesn't create a duplicate. A caller
    /// tells the two apart by comparing the returned `seq` with the log's
    /// `last_seq` before the call. The record's `published_at_ms` is raised to
    /// the previous record's if it is lower, and the returned record carries
    /// the time actually stored.
    ///
    /// Args:
    ///     topic: The topic to append to. Its log is created if needed.
    ///     record: The record to append.
    ///     durability: How firmly the record must be stored before this returns.
    ///
    /// Returns:
    ///     The stored record, with its `seq` and final publish time.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When the backend can't honour `durability`.
    ///     StorageError::Unavailable: When the backend can't store the record now.
    async fn append(
        &self,
        topic: &str,
        record: NewRecord,
        durability: Durability,
    ) -> Result<Record>;

    /// Appends several records to one topic, in order.
    ///
    /// The outcome is the same as calling `append` for each record in turn.
    /// A backend that can store a batch more cheaply than one record at a
    /// time overrides this: the file backend writes a batch with one write
    /// and, when synced, one sync. The default appends them one by one.
    ///
    /// Args:
    ///     topic: The topic to append to. Its log is created if needed.
    ///     records: The records to append, oldest first.
    ///     durability: How firmly the records must be stored before this returns.
    ///
    /// Returns:
    ///     One result per record, in the same order: the stored record, or
    ///     the error `append` would have raised for it.
    async fn append_batch(
        &self,
        topic: &str,
        records: Vec<NewRecord>,
        durability: Durability,
    ) -> Vec<Result<Record>> {
        let mut results = Vec::with_capacity(records.len());
        for record in records {
            results.push(self.append(topic, record, durability).await);
        }
        results
    }

    /// Reads records in `seq` order, starting at `from`.
    ///
    /// Args:
    ///     topic: The topic to read.
    ///     from: The first `seq` wanted. Anything below the oldest retained
    ///         record starts at the oldest retained record.
    ///     max: The most records to return.
    ///
    /// Returns:
    ///     Up to `max` consecutive records, empty for an unknown topic or when
    ///     `from` is past the end.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be read now.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    async fn read(&self, topic: &str, from: u64, max: usize) -> Result<Vec<Record>>;

    /// Reports which sequence numbers a topic log holds.
    ///
    /// Args:
    ///     topic: The topic to check.
    ///
    /// Returns:
    ///     The log's bounds, `LogBounds::EMPTY` for an unknown topic.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be read now.
    async fn bounds(&self, topic: &str) -> Result<LogBounds>;

    /// Finds the first retained record published at or after a time.
    ///
    /// Args:
    ///     topic: The topic to search.
    ///     time_ms: The time, in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     That record's `seq`, or `None` when every retained record is older.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be read now.
    async fn seq_at(&self, topic: &str, time_ms: i64) -> Result<Option<u64>>;

    /// Removes records from the front of a log until it is within a policy.
    ///
    /// Args:
    ///     topic: The topic to trim.
    ///     policy: The limits to apply.
    ///     now_ms: The current time, for the age limit.
    ///
    /// Returns:
    ///     How many records were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be changed now.
    async fn trim(&self, topic: &str, policy: &Retention, now_ms: i64) -> Result<u64>;

    /// Lists every topic that has a log, including logs whose records were
    /// all trimmed.
    ///
    /// Returns:
    ///     The topic names, sorted.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be read now.
    async fn topics(&self) -> Result<Vec<String>>;
}

/// Durable sessions, their subscriptions, and each subscription's cursor.
#[async_trait]
pub trait SubscriptionStore: Send + Sync {
    /// Loads every subscription a session holds.
    ///
    /// Args:
    ///     session_id: The session to load.
    ///
    /// Returns:
    ///     The subscriptions sorted by topic, empty for an unknown session.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be read now.
    async fn load(&self, session_id: &str) -> Result<Vec<Subscription>>;

    /// Creates or replaces a subscription, creating the session if needed.
    ///
    /// Args:
    ///     session_id: The session subscribing.
    ///     topic: The topic subscribed to.
    ///     acked_seq: The cursor to start from. Delivery resumes after it.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be changed now.
    async fn subscribe(&self, session_id: &str, topic: &str, acked_seq: u64) -> Result<()>;

    /// Removes a subscription. Removing one that doesn't exist does nothing.
    ///
    /// Args:
    ///     session_id: The session unsubscribing.
    ///     topic: The topic to drop.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be changed now.
    async fn unsubscribe(&self, session_id: &str, topic: &str) -> Result<()>;

    /// Moves a subscription's cursor forward.
    ///
    /// A value at or below the current cursor, or for a subscription that
    /// doesn't exist, is ignored. Callers pass the highest contiguous
    /// acknowledged `seq`, not each acknowledgement.
    ///
    /// Args:
    ///     session_id: The session acknowledging.
    ///     topic: The subscribed topic.
    ///     seq: The new cursor.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be changed now.
    async fn ack(&self, session_id: &str, topic: &str, seq: u64) -> Result<()>;

    /// Records that a session was active, for `expire`. Creates the session
    /// if needed.
    ///
    /// Args:
    ///     session_id: The active session.
    ///     at_ms: When it was active, in milliseconds since the Unix epoch.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be changed now.
    async fn touch(&self, session_id: &str, at_ms: i64) -> Result<()>;

    /// Removes sessions, with their subscriptions, not touched since a time.
    ///
    /// A session that was never touched counts as last touched at 0, so
    /// callers touch a session when it connects.
    ///
    /// Args:
    ///     before_ms: Sessions last touched before this time are removed.
    ///
    /// Returns:
    ///     How many sessions were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the backend can't be changed now.
    async fn expire(&self, before_ms: i64) -> Result<u64>;
}

/// A complete backend: topic logs, subscription cursors, and what it can promise.
///
/// The server holds an `Arc<dyn Storage>`, so third-party backends plug in by
/// implementing this and passing the conformance suite.
#[async_trait]
pub trait Storage: MessageLog + SubscriptionStore {
    /// Reports the backend's durability and persistence.
    ///
    /// Returns:
    ///     The backend's capabilities.
    fn capabilities(&self) -> Capabilities;

    /// Copies everything the backend holds into a new directory while it
    /// keeps serving, in a form that opens as a store of the same backend.
    ///
    /// The default refuses, for backends with nothing on local disk to copy
    /// (`MemoryStorage`) or that are backed up by other means (a database).
    ///
    /// Args:
    ///     dest: The directory to create. It must not exist yet.
    ///
    /// Returns:
    ///     What was copied.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When the backend can't be backed up this way.
    ///     StorageError::Unavailable: When `dest` exists or a file can't be
    ///         read or written.
    async fn backup(&self, dest: &Path) -> Result<BackupSummary> {
        let _ = dest;
        Err(StorageError::Unsupported(
            "this storage backend can't be backed up while running".to_string(),
        ))
    }
}
