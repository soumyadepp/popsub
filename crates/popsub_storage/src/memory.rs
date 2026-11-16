//! `MemoryStorage`, a backend that keeps logs and cursors in process memory.
//!
//! It is the simplest implementation of the traits: tests and demos use it,
//! and it serves as a pure replay buffer when nothing needs to survive a
//! restart. It never writes anywhere, so it supports only
//! `Durability::None` and reports itself as not persistent. The numbering,
//! retry and retention rules live in `LogIndex` and the session rules in
//! `SessionTable`, shared with `FileStorage`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;

use crate::error::Result;
use crate::index::LogIndex;
use crate::record::{LogBounds, NewRecord, Record, Subscription};
use crate::sessions::SessionTable;
use crate::storage::{Capabilities, Durability, MessageLog, Retention, Storage, SubscriptionStore};

/// What `MemoryStorage` reports from `capabilities`.
const CAPABILITIES: Capabilities = Capabilities {
    max_durability: Durability::None,
    persistent: false,
};

/// A backend that keeps everything in memory and loses it when dropped.
///
/// Logs and sessions each sit behind one mutex. Nothing awaits while holding
/// either, and a poisoned lock is recovered, because every change leaves the
/// data consistent before anything can panic.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    /// Topic logs by topic name, each entry holding the whole record.
    logs: Mutex<HashMap<String, LogIndex<Record>>>,
    /// Sessions and their cursors.
    sessions: Mutex<SessionTable>,
}

impl MemoryStorage {
    /// Creates an empty store.
    ///
    /// Returns:
    ///     A store with no topics and no sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the topic logs, recovering the data if a previous holder panicked.
    ///
    /// Returns:
    ///     The guard over every topic log.
    fn logs(&self) -> MutexGuard<'_, HashMap<String, LogIndex<Record>>> {
        self.logs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the sessions, recovering the data if a previous holder panicked.
    ///
    /// Returns:
    ///     The guard over every session.
    fn sessions(&self) -> MutexGuard<'_, SessionTable> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl MessageLog for MemoryStorage {
    /// Appends under the logs lock, so concurrent appends to one topic get
    /// consecutive sequence numbers.
    ///
    /// Args:
    ///     topic: The topic to append to.
    ///     record: The record to append.
    ///     durability: Must be `Durability::None`.
    ///
    /// Returns:
    ///     The stored record, or the original one for a duplicate.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When `durability` is stronger than `None`.
    async fn append(
        &self,
        topic: &str,
        record: NewRecord,
        durability: Durability,
    ) -> Result<Record> {
        CAPABILITIES.check(durability)?;
        let mut logs = self.logs();
        let log = logs.entry(topic.to_string()).or_default();
        if let Some(original) = log.duplicate_of(&record) {
            return Ok(original.value.clone());
        }
        let slot = log.next_slot(&record);
        let key = (record.publisher.clone(), record.message_id.clone());
        let stored = Record::from_new(topic, slot.seq, record, slot.published_at_ms);
        log.push(slot, stored.size(), key, stored.clone());
        Ok(stored)
    }

    /// Copies up to `max` records out of the log, starting at `from`.
    ///
    /// Args:
    ///     topic: The topic to read.
    ///     from: The first `seq` wanted.
    ///     max: The most records to return.
    ///
    /// Returns:
    ///     The records in `seq` order.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn read(&self, topic: &str, from: u64, max: usize) -> Result<Vec<Record>> {
        Ok(self.logs().get(topic).map_or_else(Vec::new, |log| {
            log.range(from, max).map(|e| e.value.clone()).collect()
        }))
    }

    /// Reports the topic's bounds.
    ///
    /// Args:
    ///     topic: The topic to check.
    ///
    /// Returns:
    ///     The bounds, `LogBounds::EMPTY` for an unknown topic.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn bounds(&self, topic: &str) -> Result<LogBounds> {
        Ok(self
            .logs()
            .get(topic)
            .map_or(LogBounds::EMPTY, LogIndex::bounds))
    }

    /// Binary searches the retained records by publish time.
    ///
    /// Args:
    ///     topic: The topic to search.
    ///     time_ms: The time to search for.
    ///
    /// Returns:
    ///     The first matching `seq`, or `None`.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn seq_at(&self, topic: &str, time_ms: i64) -> Result<Option<u64>> {
        Ok(self.logs().get(topic).and_then(|log| log.seq_at(time_ms)))
    }

    /// Drops records from the front while the policy says so.
    ///
    /// Args:
    ///     topic: The topic to trim.
    ///     policy: The limits to apply.
    ///     now_ms: The current time.
    ///
    /// Returns:
    ///     How many records were removed.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn trim(&self, topic: &str, policy: &Retention, now_ms: i64) -> Result<u64> {
        Ok(self
            .logs()
            .get_mut(topic)
            .map_or(0, |log| log.trim(policy, now_ms).len() as u64))
    }

    /// Lists the topics with a log.
    ///
    /// Returns:
    ///     The topic names, sorted.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn topics(&self) -> Result<Vec<String>> {
        let mut topics: Vec<String> = self.logs().keys().cloned().collect();
        topics.sort();
        Ok(topics)
    }
}

#[async_trait]
impl SubscriptionStore for MemoryStorage {
    /// Copies a session's cursors out.
    ///
    /// Args:
    ///     session_id: The session to load.
    ///
    /// Returns:
    ///     The subscriptions sorted by topic.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn load(&self, session_id: &str) -> Result<Vec<Subscription>> {
        Ok(self.sessions().load(session_id))
    }

    /// Sets the cursor, creating the session if needed.
    ///
    /// Args:
    ///     session_id: The session subscribing.
    ///     topic: The topic subscribed to.
    ///     acked_seq: The starting cursor.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn subscribe(&self, session_id: &str, topic: &str, acked_seq: u64) -> Result<()> {
        self.sessions().subscribe(session_id, topic, acked_seq);
        Ok(())
    }

    /// Drops the cursor if it exists.
    ///
    /// Args:
    ///     session_id: The session unsubscribing.
    ///     topic: The topic to drop.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn unsubscribe(&self, session_id: &str, topic: &str) -> Result<()> {
        self.sessions().unsubscribe(session_id, topic);
        Ok(())
    }

    /// Raises the cursor if the subscription exists and `seq` is higher.
    ///
    /// Args:
    ///     session_id: The session acknowledging.
    ///     topic: The subscribed topic.
    ///     seq: The new cursor.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn ack(&self, session_id: &str, topic: &str, seq: u64) -> Result<()> {
        self.sessions().ack(session_id, topic, seq);
        Ok(())
    }

    /// Sets the session's last activity, creating it if needed.
    ///
    /// Args:
    ///     session_id: The active session.
    ///     at_ms: When it was active.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn touch(&self, session_id: &str, at_ms: i64) -> Result<()> {
        self.sessions().touch(session_id, at_ms);
        Ok(())
    }

    /// Drops sessions last touched before `before_ms`.
    ///
    /// Args:
    ///     before_ms: The cutoff.
    ///
    /// Returns:
    ///     How many sessions were removed.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn expire(&self, before_ms: i64) -> Result<u64> {
        Ok(self.sessions().expire(before_ms))
    }
}

impl Storage for MemoryStorage {
    /// Reports that this backend holds data in memory only.
    ///
    /// Returns:
    ///     `Durability::None` and not persistent.
    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }
}
