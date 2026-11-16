//! `MemoryStorage`, a backend that keeps logs and cursors in process memory.
//!
//! It is the reference implementation of the traits: tests and demos use it,
//! and it serves as a pure replay buffer when nothing needs to survive a
//! restart. It never writes anywhere, so it supports only
//! `Durability::None` and reports itself as not persistent.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;

use crate::error::Result;
use crate::record::{LogBounds, NewRecord, Record, Subscription};
use crate::storage::{Capabilities, Durability, MessageLog, Retention, Storage, SubscriptionStore};

/// What `MemoryStorage` reports from `capabilities`.
const CAPABILITIES: Capabilities = Capabilities {
    max_durability: Durability::None,
    persistent: false,
};

/// One topic's records and the bookkeeping needed to append to it.
#[derive(Debug, Default)]
struct TopicLog {
    /// Retained records, oldest first, with consecutive `seq` values.
    records: VecDeque<Record>,
    /// The highest `seq` ever appended, kept when records are trimmed.
    last_seq: u64,
    /// The highest `published_at_ms` ever appended, kept when records are trimmed.
    last_published_at_ms: i64,
    /// The total `Record::size` of `records`.
    bytes: u64,
    /// Maps `(publisher, message_id)` to `seq` for every retained record.
    ids: HashMap<(String, String), u64>,
}

impl TopicLog {
    /// Reports which sequence numbers this log holds.
    ///
    /// Returns:
    ///     The bounds of the retained records.
    fn bounds(&self) -> LogBounds {
        LogBounds {
            first_seq: self.records.front().map_or(self.last_seq + 1, |r| r.seq),
            last_seq: self.last_seq,
        }
    }

    /// Removes the oldest record and its id lookup.
    ///
    /// Returns:
    ///     False when the log was already empty.
    fn pop_oldest(&mut self) -> bool {
        let Some(record) = self.records.pop_front() else {
            return false;
        };
        self.bytes -= record.size();
        self.ids.remove(&(record.publisher, record.message_id));
        true
    }
}

/// One session's last activity and its cursors.
#[derive(Debug, Default)]
struct Session {
    /// When the session was last touched, 0 if never.
    last_seen_ms: i64,
    /// Maps each subscribed topic to its `acked_seq`.
    cursors: HashMap<String, u64>,
}

/// A backend that keeps everything in memory and loses it when dropped.
///
/// Logs and sessions each sit behind one mutex. Nothing awaits while holding
/// either, and a poisoned lock is recovered, because every change leaves the
/// data consistent before anything can panic.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    /// Topic logs by topic name.
    logs: Mutex<HashMap<String, TopicLog>>,
    /// Sessions by session id.
    sessions: Mutex<HashMap<String, Session>>,
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
    fn logs(&self) -> MutexGuard<'_, HashMap<String, TopicLog>> {
        self.logs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the sessions, recovering the data if a previous holder panicked.
    ///
    /// Returns:
    ///     The guard over every session.
    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Session>> {
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
        let key = (record.publisher.clone(), record.message_id.clone());
        if let Some(&seq) = log.ids.get(&key) {
            let first = log.bounds().first_seq;
            let index = usize::try_from(seq - first).unwrap_or(usize::MAX);
            if let Some(original) = log.records.get(index) {
                return Ok(original.clone());
            }
        }
        let seq = log.last_seq + 1;
        let published_at_ms = record.published_at_ms.max(log.last_published_at_ms);
        let stored = Record::from_new(topic, seq, record, published_at_ms);
        log.bytes += stored.size();
        log.last_seq = seq;
        log.last_published_at_ms = published_at_ms;
        log.ids.insert(key, seq);
        log.records.push_back(stored.clone());
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
        let logs = self.logs();
        let Some(log) = logs.get(topic) else {
            return Ok(Vec::new());
        };
        let first = log.bounds().first_seq;
        let skip = usize::try_from(from.saturating_sub(first)).unwrap_or(usize::MAX);
        Ok(log.records.iter().skip(skip).take(max).cloned().collect())
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
            .map_or(LogBounds::EMPTY, TopicLog::bounds))
    }

    /// Binary searches the retained records by publish time, which never
    /// decreases along the log.
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
        let logs = self.logs();
        let Some(log) = logs.get(topic) else {
            return Ok(None);
        };
        let index = log.records.partition_point(|r| r.published_at_ms < time_ms);
        Ok(log.records.get(index).map(|r| r.seq))
    }

    /// Pops records from the front while `Retention::must_trim` says so.
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
        let mut logs = self.logs();
        let Some(log) = logs.get_mut(topic) else {
            return Ok(0);
        };
        let mut removed = 0;
        while let Some(oldest) = log.records.front()
            && policy.must_trim(
                log.records.len() as u64,
                log.bytes,
                oldest.published_at_ms,
                now_ms,
            )
            && log.pop_oldest()
        {
            removed += 1;
        }
        Ok(removed)
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
        let sessions = self.sessions();
        let Some(session) = sessions.get(session_id) else {
            return Ok(Vec::new());
        };
        let mut subscriptions: Vec<Subscription> = session
            .cursors
            .iter()
            .map(|(topic, &acked_seq)| Subscription {
                session_id: session_id.to_string(),
                topic: topic.clone(),
                acked_seq,
            })
            .collect();
        subscriptions.sort_by(|a, b| a.topic.cmp(&b.topic));
        Ok(subscriptions)
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
        self.sessions()
            .entry(session_id.to_string())
            .or_default()
            .cursors
            .insert(topic.to_string(), acked_seq);
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
        if let Some(session) = self.sessions().get_mut(session_id) {
            session.cursors.remove(topic);
        }
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
        if let Some(session) = self.sessions().get_mut(session_id)
            && let Some(cursor) = session.cursors.get_mut(topic)
        {
            *cursor = (*cursor).max(seq);
        }
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
        let mut sessions = self.sessions();
        let session = sessions.entry(session_id.to_string()).or_default();
        session.last_seen_ms = session.last_seen_ms.max(at_ms);
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
        let mut sessions = self.sessions();
        let before = sessions.len();
        sessions.retain(|_, session| session.last_seen_ms >= before_ms);
        Ok(before.saturating_sub(sessions.len()) as u64)
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
