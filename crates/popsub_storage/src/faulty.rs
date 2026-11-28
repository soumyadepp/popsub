//! `FaultyStorage`, a wrapper that makes any backend fail on demand, for
//! testing how the broker and server behave when storage breaks (a full
//! disk, a lost volume) without breaking a real one.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use crate::error::{Result, StorageError};
use crate::record::{LogBounds, NewRecord, Record, Subscription};
use crate::storage::{
    BackupSummary, Capabilities, Durability, MessageLog, Retention, Storage, SubscriptionStore,
};

/// Wraps a backend and fails every append with `StorageError::Unavailable`
/// while switched to failing. Everything else is passed through.
///
/// Clones share the switch, so a test can keep one and hand another to the
/// broker.
#[derive(Clone)]
pub struct FaultyStorage {
    /// The backend being wrapped.
    inner: Arc<dyn Storage>,
    /// Whether appends fail.
    failing: Arc<AtomicBool>,
}

impl FaultyStorage {
    /// Wraps a backend, working normally until `set_failing(true)`.
    ///
    /// Args:
    ///     inner: The backend.
    ///
    /// Returns:
    ///     The wrapper.
    pub fn new(inner: Arc<dyn Storage>) -> Self {
        Self {
            inner,
            failing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Switches appends between failing and working.
    ///
    /// Args:
    ///     failing: Whether appends fail from now on.
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }
}

#[async_trait]
impl MessageLog for FaultyStorage {
    /// Appends through the wrapped backend, or fails while switched to failing.
    ///
    /// Args:
    ///     topic: The topic.
    ///     record: The record.
    ///     durability: How firmly to store it.
    ///
    /// Returns:
    ///     The stored record.
    ///
    /// Raises:
    ///     StorageError::Unavailable: While switched to failing, or when the
    ///         wrapped backend fails.
    ///     StorageError::Unsupported: When the wrapped backend can't honour `durability`.
    async fn append(
        &self,
        topic: &str,
        record: NewRecord,
        durability: Durability,
    ) -> Result<Record> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable(
                "appends are switched off for this test".to_string(),
            ));
        }
        self.inner.append(topic, record, durability).await
    }

    /// Reads through the wrapped backend.
    ///
    /// Args:
    ///     topic: The topic.
    ///     from: The first `seq` to read.
    ///     max: The most records to return.
    ///
    /// Returns:
    ///     The records.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn read(&self, topic: &str, from: u64, max: usize) -> Result<Vec<Record>> {
        self.inner.read(topic, from, max).await
    }

    /// Reads the bounds through the wrapped backend.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The bounds.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn bounds(&self, topic: &str) -> Result<LogBounds> {
        self.inner.bounds(topic).await
    }

    /// Looks up a time through the wrapped backend.
    ///
    /// Args:
    ///     topic: The topic.
    ///     time_ms: The time.
    ///
    /// Returns:
    ///     The first `seq` at or after it.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn seq_at(&self, topic: &str, time_ms: i64) -> Result<Option<u64>> {
        self.inner.seq_at(topic, time_ms).await
    }

    /// Trims through the wrapped backend.
    ///
    /// Args:
    ///     topic: The topic.
    ///     policy: The retention policy.
    ///     now_ms: The current time.
    ///
    /// Returns:
    ///     How many records were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn trim(&self, topic: &str, policy: &Retention, now_ms: i64) -> Result<u64> {
        self.inner.trim(topic, policy, now_ms).await
    }

    /// Lists topics through the wrapped backend.
    ///
    /// Returns:
    ///     The topic names.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn topics(&self) -> Result<Vec<String>> {
        self.inner.topics().await
    }

    /// Lists creators through the wrapped backend.
    ///
    /// Returns:
    ///     The wrapped backend's `(topic, creator)` pairs.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn creators(&self) -> Result<Vec<(String, String)>> {
        self.inner.creators().await
    }
}

#[async_trait]
impl SubscriptionStore for FaultyStorage {
    /// Loads a session through the wrapped backend.
    ///
    /// Args:
    ///     session_id: The session.
    ///
    /// Returns:
    ///     Its subscriptions.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn load(&self, session_id: &str) -> Result<Vec<Subscription>> {
        self.inner.load(session_id).await
    }

    /// Stores a subscription through the wrapped backend.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///     acked_seq: The cursor.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn subscribe(&self, session_id: &str, topic: &str, acked_seq: u64) -> Result<()> {
        self.inner.subscribe(session_id, topic, acked_seq).await
    }

    /// Removes a subscription through the wrapped backend.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn unsubscribe(&self, session_id: &str, topic: &str) -> Result<()> {
        self.inner.unsubscribe(session_id, topic).await
    }

    /// Moves a cursor through the wrapped backend.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///     seq: The acknowledged `seq`.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn ack(&self, session_id: &str, topic: &str, seq: u64) -> Result<()> {
        self.inner.ack(session_id, topic, seq).await
    }

    /// Marks a session as used through the wrapped backend.
    ///
    /// Args:
    ///     session_id: The session.
    ///     at_ms: When.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn touch(&self, session_id: &str, at_ms: i64) -> Result<()> {
        self.inner.touch(session_id, at_ms).await
    }

    /// Expires sessions through the wrapped backend.
    ///
    /// Args:
    ///     before_ms: Sessions last used before this are removed.
    ///
    /// Returns:
    ///     How many were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the wrapped backend fails.
    async fn expire(&self, before_ms: i64) -> Result<u64> {
        self.inner.expire(before_ms).await
    }
}

#[async_trait]
impl Storage for FaultyStorage {
    /// Reports the wrapped backend's capabilities.
    ///
    /// Returns:
    ///     The capabilities.
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    /// Backs up the wrapped backend. Failing appends don't affect a backup.
    ///
    /// Args:
    ///     dest: The directory to create.
    ///
    /// Returns:
    ///     What the wrapped backend copied.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When the wrapped backend can't be backed up.
    ///     StorageError::Unavailable: When the wrapped backend's backup fails.
    async fn backup(&self, dest: &Path) -> Result<BackupSummary> {
        self.inner.backup(dest).await
    }
}
