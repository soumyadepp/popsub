//! `FileStorage`, the built-in backend that keeps topic logs and sessions in
//! a directory on local disk.
//!
//! Layout under the root directory:
//!
//! - `LOCK`: held with an exclusive file lock while the store is open, so a
//!   second process (or a second `FileStorage` in one process) can't open
//!   the same directory.
//! - `topics/t<hex>/`: one directory per topic, named by the topic's bytes
//!   in hex so any topic name is a safe file name. See `topic`.
//! - `sessions.log`: the session write-ahead log. See `sessions`.
//!
//! Every topic and the session log sit behind their own mutex. The file I/O
//! is synchronous and runs on Tokio's blocking thread pool, so storage calls
//! never stall the async workers. Each record's location is indexed in
//! memory when the store opens, which costs a scan of every segment at
//! startup and some memory per retained record.
//!
//! `Durability::None` and `Durability::Buffered` both write to the operating
//! system on every append (survives a process crash, not a power loss).
//! `Durability::Synced` also flushes the segment to disk before the append
//! returns. Each synced append is flushed on its own; flushes are not yet
//! grouped across appends.

mod codec;
mod sessions;
mod topic;

use std::collections::HashMap;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;

use crate::error::{Result, StorageError};
use crate::record::{LogBounds, NewRecord, Record, Subscription};
use crate::storage::{Capabilities, Durability, MessageLog, Retention, Storage, SubscriptionStore};

use sessions::SessionLog;
use topic::TopicFiles;

/// What `FileStorage` reports from `capabilities`.
const CAPABILITIES: Capabilities = Capabilities {
    max_durability: Durability::Synced,
    persistent: true,
};

/// The default size at which a topic starts a new segment file: 64 MiB.
pub const DEFAULT_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

/// The longest topic name, in bytes, the file backend accepts. Topic
/// directories are named by the name in hex, and most file systems limit a
/// name to 255 bytes.
pub const MAX_TOPIC_BYTES: usize = 120;

/// Options for opening a `FileStorage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOptions {
    /// The size at which a topic starts a new segment file. Smaller segments
    /// let trimming free space sooner; larger ones mean fewer files.
    pub segment_bytes: u64,
}

impl Default for FileOptions {
    /// Builds the default options: 64 MiB segments.
    ///
    /// Returns:
    ///     The defaults.
    fn default() -> Self {
        Self {
            segment_bytes: DEFAULT_SEGMENT_BYTES,
        }
    }
}

/// Turns an I/O error into a storage error that names the file.
///
/// Args:
///     error: The I/O error.
///     path: The file or directory involved.
///
/// Returns:
///     `StorageError::Unavailable` with both in the message.
pub(crate) fn io_error(error: std::io::Error, path: &Path) -> StorageError {
    StorageError::Unavailable(format!("{}: {error}", path.display()))
}

/// Flushes a directory, so files just created or renamed in it survive a
/// power loss. A no-op on platforms that can't open a directory as a file.
///
/// Args:
///     dir: The directory.
///
/// Raises:
///     StorageError::Unavailable: When the directory opens but can't be synced.
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    match File::open(dir) {
        Ok(handle) => handle.sync_all().map_err(|e| io_error(e, dir)),
        Err(_) => Ok(()),
    }
}

/// Builds the directory name for a topic.
///
/// Args:
///     topic: The topic name.
///
/// Returns:
///     `t` followed by the name's bytes in lowercase hex.
fn topic_dir_name(topic: &str) -> String {
    let mut name = String::with_capacity(1 + topic.len() * 2);
    name.push('t');
    for byte in topic.bytes() {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// Recovers a topic name from its directory name.
///
/// Args:
///     name: A directory name made by `topic_dir_name`.
///
/// Returns:
///     The topic, or `None` for a name that wasn't made that way.
fn topic_from_dir_name(name: &str) -> Option<String> {
    let hex = name.strip_prefix('t')?;
    if hex.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// Locks a mutex, recovering the data if a previous holder panicked.
///
/// Args:
///     mutex: The mutex.
///
/// Returns:
///     The guard.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The open store, shared with the blocking tasks that do its I/O.
#[derive(Debug)]
struct Inner {
    /// The directory topic directories go in.
    topics_dir: PathBuf,
    /// Open topics by name.
    topics: Mutex<HashMap<String, Arc<Mutex<TopicFiles>>>>,
    /// The session log and table.
    sessions: Mutex<SessionLog>,
    /// Segment size and other options.
    options: FileOptions,
    /// Holds the directory's exclusive lock until the store is dropped.
    _lock: File,
}

impl Inner {
    /// Returns a topic, opening its directory first if `create` is set and it
    /// doesn't exist yet.
    ///
    /// Args:
    ///     name: The topic.
    ///     create: Whether to create a topic that doesn't exist.
    ///
    /// Returns:
    ///     The topic, or `None` when it doesn't exist and `create` is false.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When the name is longer than `MAX_TOPIC_BYTES`.
    ///     StorageError::Unavailable: When its directory can't be created.
    fn topic(&self, name: &str, create: bool) -> Result<Option<Arc<Mutex<TopicFiles>>>> {
        let mut topics = lock(&self.topics);
        if let Some(topic) = topics.get(name) {
            return Ok(Some(topic.clone()));
        }
        if !create {
            return Ok(None);
        }
        if name.len() > MAX_TOPIC_BYTES {
            return Err(StorageError::Unsupported(format!(
                "topic names longer than {MAX_TOPIC_BYTES} bytes"
            )));
        }
        let dir = self.topics_dir.join(topic_dir_name(name));
        let topic = Arc::new(Mutex::new(TopicFiles::open(
            &dir,
            name,
            self.options.segment_bytes,
        )?));
        topics.insert(name.to_string(), topic.clone());
        Ok(Some(topic))
    }
}

/// A backend that keeps topic logs and sessions in a local directory and
/// survives restarts.
///
/// Cloning shares the same open store.
#[derive(Debug, Clone)]
pub struct FileStorage {
    /// The open store.
    inner: Arc<Inner>,
}

impl FileStorage {
    /// Opens (or creates) a store in a directory, with the default options.
    ///
    /// Args:
    ///     root: The directory. Created if it doesn't exist.
    ///
    /// Returns:
    ///     The open store.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the directory can't be created or
    ///         read, or another process already has it open.
    ///     StorageError::Corrupt: When a topic's files are damaged anywhere
    ///         but at the end of its newest segment.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with(root, FileOptions::default()).await
    }

    /// Opens (or creates) a store in a directory.
    ///
    /// Every topic is scanned and its index rebuilt, and an interrupted write
    /// at the end of a topic's newest segment is cut off.
    ///
    /// Args:
    ///     root: The directory. Created if it doesn't exist.
    ///     options: Segment size and other options.
    ///
    /// Returns:
    ///     The open store.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the directory can't be created or
    ///         read, or another process already has it open.
    ///     StorageError::Corrupt: When a topic's files are damaged anywhere
    ///         but at the end of its newest segment.
    pub async fn open_with(root: impl Into<PathBuf>, options: FileOptions) -> Result<Self> {
        let root = root.into();
        let inner = tokio::task::spawn_blocking(move || open_blocking(&root, options))
            .await
            .map_err(|e| StorageError::Other(format!("opening the store panicked: {e}")))??;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Runs a closure against the store on Tokio's blocking thread pool.
    ///
    /// Args:
    ///     work: The synchronous work.
    ///
    /// Returns:
    ///     What `work` returns.
    ///
    /// Raises:
    ///     StorageError::Other: When `work` panics.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Inner) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || work(&inner))
            .await
            .map_err(|e| StorageError::Other(format!("a storage task panicked: {e}")))?
    }
}

/// Opens the store's directory: takes the lock, then opens every topic and
/// the session log.
///
/// Args:
///     root: The directory.
///     options: Segment size and other options.
///
/// Returns:
///     The open store.
///
/// Raises:
///     StorageError::Unavailable: When a directory or file can't be created
///         or read, or the lock is held by someone else.
///     StorageError::Corrupt: When a topic's files are damaged.
fn open_blocking(root: &Path, options: FileOptions) -> Result<Inner> {
    let topics_dir = root.join("topics");
    fs::create_dir_all(&topics_dir).map_err(|e| io_error(e, &topics_dir))?;
    let lock_path = root.join("LOCK");
    let lock_file = File::create(&lock_path).map_err(|e| io_error(e, &lock_path))?;
    match lock_file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(StorageError::Unavailable(format!(
                "{} is already open in another process",
                root.display()
            )));
        }
        Err(TryLockError::Error(e)) => return Err(io_error(e, &lock_path)),
    }
    let mut topics = HashMap::new();
    for entry in fs::read_dir(&topics_dir).map_err(|e| io_error(e, &topics_dir))? {
        let path = entry.map_err(|e| io_error(e, &topics_dir))?.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(topic_from_dir_name)
        else {
            continue;
        };
        let topic = TopicFiles::open(&path, &name, options.segment_bytes)?;
        topics.insert(name, Arc::new(Mutex::new(topic)));
    }
    let sessions = SessionLog::open(&root.join("sessions.log"))?;
    Ok(Inner {
        topics_dir,
        topics: Mutex::new(topics),
        sessions: Mutex::new(sessions),
        options,
        _lock: lock_file,
    })
}

#[async_trait]
impl MessageLog for FileStorage {
    /// Appends to the topic's newest segment at the requested durability.
    ///
    /// Args:
    ///     topic: The topic to append to.
    ///     record: The record to append.
    ///     durability: How firmly to store it.
    ///
    /// Returns:
    ///     The stored record, or the original one for a duplicate.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When the topic name is longer than `MAX_TOPIC_BYTES`.
    ///     StorageError::Unavailable: When the segment can't be written or synced.
    ///     StorageError::Corrupt: When a duplicate's original can't be read back.
    async fn append(
        &self,
        topic: &str,
        record: NewRecord,
        durability: Durability,
    ) -> Result<Record> {
        CAPABILITIES.check(durability)?;
        let topic = topic.to_string();
        self.blocking(move |inner| {
            let files = inner.topic(&topic, true)?.expect("created when missing");
            let mut files = lock(&files);
            files.append(record, durability)
        })
        .await
    }

    /// Reads records from the topic's segments.
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
    ///     StorageError::Unavailable: When a segment can't be read.
    ///     StorageError::Corrupt: When a record fails its CRC or doesn't decode.
    /// Appends a batch with one trip to the blocking pool and one write per segment.
    ///
    /// Args:
    ///     topic: The topic to append to.
    ///     records: The records to append, oldest first.
    ///     durability: How firmly the records must be stored before this returns.
    ///
    /// Returns:
    ///     One result per record. When the topic can't be opened or the
    ///     durability isn't supported, every result is that error.
    async fn append_batch(
        &self,
        topic: &str,
        records: Vec<NewRecord>,
        durability: Durability,
    ) -> Vec<Result<Record>> {
        let count = records.len();
        if let Err(e) = CAPABILITIES.check(durability) {
            return vec![Err(e); count];
        }
        let topic = topic.to_string();
        let appended = self
            .blocking(move |inner| {
                let files = inner.topic(&topic, true)?.expect("created when missing");
                let mut files = lock(&files);
                Ok(files.append_batch(records, durability))
            })
            .await;
        appended.unwrap_or_else(|e| vec![Err(e); count])
    }

    async fn read(&self, topic: &str, from: u64, max: usize) -> Result<Vec<Record>> {
        let topic = topic.to_string();
        self.blocking(move |inner| match inner.topic(&topic, false)? {
            Some(files) => lock(&files).read(from, max),
            None => Ok(Vec::new()),
        })
        .await
    }

    /// Reports the topic's bounds from the in-memory index.
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
            .inner
            .topic(topic, false)?
            .map_or(LogBounds::EMPTY, |files| lock(&files).bounds()))
    }

    /// Searches the in-memory index by publish time.
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
        Ok(self
            .inner
            .topic(topic, false)?
            .and_then(|files| lock(&files).seq_at(time_ms)))
    }

    /// Trims the topic and deletes segments left with no retained records.
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
    ///     StorageError::Unavailable: When the head can't be written or a
    ///         segment can't be deleted.
    async fn trim(&self, topic: &str, policy: &Retention, now_ms: i64) -> Result<u64> {
        let Some(files) = self.inner.topic(topic, false)? else {
            return Ok(0);
        };
        let (removed, persist) = lock(&files).trim_index(policy, now_ms);
        if persist {
            self.blocking(move |_| lock(&files).persist_trim()).await?;
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
        let mut topics: Vec<String> = lock(&self.inner.topics).keys().cloned().collect();
        topics.sort();
        Ok(topics)
    }
}

#[async_trait]
impl SubscriptionStore for FileStorage {
    /// Reads a session's subscriptions from the in-memory table.
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
        Ok(lock(&self.inner.sessions).load(session_id))
    }

    /// Logs and applies a subscription.
    ///
    /// Args:
    ///     session_id: The session subscribing.
    ///     topic: The topic subscribed to.
    ///     acked_seq: The starting cursor.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the session log can't be written.
    async fn subscribe(&self, session_id: &str, topic: &str, acked_seq: u64) -> Result<()> {
        let (session_id, topic) = (session_id.to_string(), topic.to_string());
        self.blocking(move |inner| lock(&inner.sessions).subscribe(&session_id, &topic, acked_seq))
            .await
    }

    /// Logs and applies an unsubscribe.
    ///
    /// Args:
    ///     session_id: The session unsubscribing.
    ///     topic: The topic to drop.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the session log can't be written.
    async fn unsubscribe(&self, session_id: &str, topic: &str) -> Result<()> {
        let (session_id, topic) = (session_id.to_string(), topic.to_string());
        self.blocking(move |inner| lock(&inner.sessions).unsubscribe(&session_id, &topic))
            .await
    }

    /// Logs and applies a cursor move, if it moves.
    ///
    /// Args:
    ///     session_id: The session acknowledging.
    ///     topic: The subscribed topic.
    ///     seq: The new cursor.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the session log can't be written.
    async fn ack(&self, session_id: &str, topic: &str, seq: u64) -> Result<()> {
        let (session_id, topic) = (session_id.to_string(), topic.to_string());
        self.blocking(move |inner| lock(&inner.sessions).ack(&session_id, &topic, seq))
            .await
    }

    /// Logs and applies a session's activity.
    ///
    /// Args:
    ///     session_id: The active session.
    ///     at_ms: When it was active.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the session log can't be written.
    async fn touch(&self, session_id: &str, at_ms: i64) -> Result<()> {
        let session_id = session_id.to_string();
        self.blocking(move |inner| lock(&inner.sessions).touch(&session_id, at_ms))
            .await
    }

    /// Logs and applies an expiry, if it removes anything.
    ///
    /// Args:
    ///     before_ms: The cutoff.
    ///
    /// Returns:
    ///     How many sessions were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the session log can't be written.
    async fn expire(&self, before_ms: i64) -> Result<u64> {
        self.blocking(move |inner| lock(&inner.sessions).expire(before_ms))
            .await
    }
}

impl Storage for FileStorage {
    /// Reports that this backend persists data and can flush to disk.
    ///
    /// Returns:
    ///     `Durability::Synced` and persistent.
    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }
}

#[cfg(test)]
mod tests;
