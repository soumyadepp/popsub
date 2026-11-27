//! The session log: `FileStorage`'s write-ahead log of session changes.
//!
//! Every change to a session (subscribe, unsubscribe, a cursor moving, a
//! touch, an expiry) is appended to `sessions.log` as a frame before it is
//! applied to the in-memory `SessionTable`. Opening replays the log. A bad
//! frame ends the replay and the file is cut back to the last good one,
//! since the log is only ever appended to and a damaged frame can only be
//! an interrupted write; at worst the most recent change is lost, which for
//! a cursor means resending records the client already acknowledged.
//!
//! Writes reach the operating system but aren't flushed to disk, so they
//! survive the process crashing but not a power loss. When the log holds
//! many more operations than there are live subscriptions, it is compacted:
//! the current state is written to a temporary file, flushed, and renamed
//! over the log.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, StorageError};
use crate::file::codec::{self, Decoded};
use crate::file::{io_error, sync_dir};
use crate::record::Subscription;
use crate::sessions::SessionTable;

/// The fewest operations in the log before compaction is considered.
const COMPACT_AFTER: usize = 1000;

/// One logged change to the session table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// `SessionTable::subscribe`.
    Subscribe {
        /// The session.
        session_id: String,
        /// The topic.
        topic: String,
        /// The starting cursor.
        acked_seq: u64,
    },
    /// `SessionTable::unsubscribe`.
    Unsubscribe {
        /// The session.
        session_id: String,
        /// The topic.
        topic: String,
    },
    /// `SessionTable::ack`.
    Ack {
        /// The session.
        session_id: String,
        /// The topic.
        topic: String,
        /// The new cursor.
        seq: u64,
    },
    /// `SessionTable::touch`.
    Touch {
        /// The session.
        session_id: String,
        /// When it was active.
        at_ms: i64,
    },
    /// `SessionTable::expire`.
    Expire {
        /// The cutoff.
        before_ms: i64,
    },
}

impl Op {
    /// Applies the change to a table.
    ///
    /// Args:
    ///     table: The table to change.
    fn apply(&self, table: &mut SessionTable) {
        match self {
            Op::Subscribe {
                session_id,
                topic,
                acked_seq,
            } => table.subscribe(session_id, topic, *acked_seq),
            Op::Unsubscribe { session_id, topic } => table.unsubscribe(session_id, topic),
            Op::Ack {
                session_id,
                topic,
                seq,
            } => {
                table.ack(session_id, topic, *seq);
            }
            Op::Touch { session_id, at_ms } => table.touch(session_id, *at_ms),
            Op::Expire { before_ms } => {
                table.expire(*before_ms);
            }
        }
    }
}

/// The session table and the log it is rebuilt from.
#[derive(Debug)]
pub(crate) struct SessionLog {
    /// The log file.
    path: PathBuf,
    /// An append handle on it.
    file: File,
    /// The current state.
    table: SessionTable,
    /// How many operations the log file holds.
    ops: usize,
}

impl SessionLog {
    /// Opens the log, creating it if needed, and replays it.
    ///
    /// Args:
    ///     path: The log file.
    ///
    /// Returns:
    ///     The log with its table rebuilt.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the file can't be read, truncated or opened.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io_error(e, path)),
        };
        let mut table = SessionTable::default();
        let mut ops = 0;
        let mut offset = 0;
        loop {
            match codec::decode(&bytes, offset) {
                Decoded::End => break,
                Decoded::Frame { body, next } => match serde_json::from_slice::<Op>(body) {
                    Ok(op) => {
                        op.apply(&mut table);
                        ops += 1;
                        offset = next;
                    }
                    Err(_) => break,
                },
                Decoded::Torn => break,
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| io_error(e, path))?;
        if offset < bytes.len() {
            file.set_len(offset as u64).map_err(|e| io_error(e, path))?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            table,
            ops,
        })
    }

    /// Writes an operation to the log, then applies it.
    ///
    /// Args:
    ///     op: The change.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When it can't be written. Nothing is applied.
    fn record(&mut self, op: Op) -> Result<()> {
        let body = serde_json::to_vec(&op)
            .map_err(|e| StorageError::Other(format!("encoding a session change: {e}")))?;
        self.file
            .write_all(&codec::encode(&body))
            .map_err(|e| io_error(e, &self.path))?;
        op.apply(&mut self.table);
        self.ops += 1;
        self.compact_if_large()
    }

    /// Rewrites the log as the current state once it has grown well past it.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the compacted file can't be written
    ///         or renamed into place. The old log is left as it was.
    fn compact_if_large(&mut self) -> Result<()> {
        let snapshot = self.table.snapshot();
        let live: usize = snapshot.iter().map(|(_, _, subs)| subs.len() + 1).sum();
        if self.ops < COMPACT_AFTER || self.ops < live * 4 {
            return Ok(());
        }
        let temp = self.path.with_extension("log.tmp");
        let ops = write_compacted(&temp, snapshot)?;
        fs::rename(&temp, &self.path).map_err(|e| io_error(e, &self.path))?;
        if let Some(dir) = self.path.parent() {
            sync_dir(dir)?;
        }
        self.file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| io_error(e, &self.path))?;
        self.ops = ops;
        Ok(())
    }

    /// Captures every session's state for a backup. Call it with the log locked.
    ///
    /// Returns:
    ///     Each session id with its last activity and subscriptions.
    pub(crate) fn snapshot(&self) -> SessionSnapshot {
        self.table.snapshot()
    }

    /// Lists a session's subscriptions.
    ///
    /// Args:
    ///     session_id: The session.
    ///
    /// Returns:
    ///     The subscriptions sorted by topic.
    pub(crate) fn load(&self, session_id: &str) -> Vec<Subscription> {
        self.table.load(session_id)
    }

    /// Logs and applies `SessionTable::subscribe`.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///     acked_seq: The starting cursor.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the change can't be written.
    pub(crate) fn subscribe(
        &mut self,
        session_id: &str,
        topic: &str,
        acked_seq: u64,
    ) -> Result<()> {
        self.record(Op::Subscribe {
            session_id: session_id.to_string(),
            topic: topic.to_string(),
            acked_seq,
        })
    }

    /// Logs and applies `SessionTable::unsubscribe`.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the change can't be written.
    pub(crate) fn unsubscribe(&mut self, session_id: &str, topic: &str) -> Result<()> {
        self.record(Op::Unsubscribe {
            session_id: session_id.to_string(),
            topic: topic.to_string(),
        })
    }

    /// Logs and applies a cursor move, skipping the write when it wouldn't move.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///     seq: The new cursor.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the change can't be written.
    pub(crate) fn ack(&mut self, session_id: &str, topic: &str, seq: u64) -> Result<()> {
        if self
            .table
            .cursor(session_id, topic)
            .is_none_or(|cursor| cursor >= seq)
        {
            return Ok(());
        }
        self.record(Op::Ack {
            session_id: session_id.to_string(),
            topic: topic.to_string(),
            seq,
        })
    }

    /// Logs and applies `SessionTable::touch`.
    ///
    /// Args:
    ///     session_id: The session.
    ///     at_ms: When it was active.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the change can't be written.
    pub(crate) fn touch(&mut self, session_id: &str, at_ms: i64) -> Result<()> {
        self.record(Op::Touch {
            session_id: session_id.to_string(),
            at_ms,
        })
    }

    /// Removes idle sessions, logging the expiry only when it removes some.
    ///
    /// Args:
    ///     before_ms: The cutoff.
    ///
    /// Returns:
    ///     How many sessions were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the change can't be written.
    pub(crate) fn expire(&mut self, before_ms: i64) -> Result<u64> {
        let removed = self.table.count_expired(before_ms);
        if removed > 0 {
            self.record(Op::Expire { before_ms })?;
        }
        Ok(removed)
    }

    /// How many operations the log file holds, for tests of compaction.
    ///
    /// Returns:
    ///     The count.
    #[cfg(test)]
    pub(crate) fn ops(&self) -> usize {
        self.ops
    }
}

/// Every session's id, last activity time and subscriptions.
pub(crate) type SessionSnapshot = Vec<(String, i64, Vec<Subscription>)>;

/// Writes a session log holding exactly the given state, one `touch` per
/// session followed by a `subscribe` per subscription, and flushes it.
///
/// Used for compaction and for backups, so both produce a log that `open`
/// replays to the same table.
///
/// Args:
///     path: The file to create or replace.
///     snapshot: The state to write.
///
/// Returns:
///     How many operations were written.
///
/// Raises:
///     StorageError::Unavailable: When the file can't be written or flushed.
///     StorageError::Other: When an operation can't be encoded.
pub(crate) fn write_compacted(path: &Path, snapshot: SessionSnapshot) -> Result<usize> {
    let mut out = File::create(path).map_err(|e| io_error(e, path))?;
    let mut ops = 0;
    for (session_id, at_ms, subscriptions) in snapshot {
        let mut write = |op: Op| -> Result<()> {
            let body = serde_json::to_vec(&op)
                .map_err(|e| StorageError::Other(format!("encoding a session change: {e}")))?;
            out.write_all(&codec::encode(&body))
                .map_err(|e| io_error(e, path))?;
            ops += 1;
            Ok(())
        };
        write(Op::Touch {
            session_id: session_id.clone(),
            at_ms,
        })?;
        for subscription in subscriptions {
            write(Op::Subscribe {
                session_id: session_id.clone(),
                topic: subscription.topic,
                acked_seq: subscription.acked_seq,
            })?;
        }
    }
    out.sync_all().map_err(|e| io_error(e, path))?;
    Ok(ops)
}
