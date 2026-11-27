//! Live backups of a `FileStorage`: a copy of every topic and the session log
//! that opens as a store, taken while the store keeps serving.
//!
//! The session log is captured first and the topics after it. Cursors only
//! move forward and logs only grow, so every cursor in the copy points at or
//! before its topic's last `seq` in the copy, as it would in a store that was
//! simply stopped. Each topic and the session log is captured under its own
//! lock and copied after the lock is released, so publishing and
//! acknowledging carry on while the files are copied. Topics are not captured
//! at one instant together: a record published to one topic during the
//! backup may be in the copy while a later record on another topic isn't.
//!
//! The copy is written to `<dest>.partial` and renamed to `dest` once
//! everything in it is flushed, so `dest` either holds a complete backup or
//! doesn't exist.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Result, StorageError};
use crate::file::sessions::write_compacted;
use crate::file::{Inner, io_error, lock, sync_dir, topic_dir_name};
use crate::storage::BackupSummary;

/// Builds the path a backup is written to before it is renamed into place.
///
/// Args:
///     dest: The backup's final directory.
///
/// Returns:
///     `dest` with `.partial` appended to its name.
fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

/// Backs a store up into `dest`, removing the partial copy if anything fails.
///
/// Args:
///     inner: The open store.
///     dest: The directory to create. Its parent is created if needed.
///
/// Returns:
///     What was copied.
///
/// Raises:
///     StorageError::Unavailable: When `dest` or its partial copy already
///         exists, or a file can't be read, written, flushed or renamed.
///     StorageError::Other: When a session change can't be encoded.
pub(crate) fn backup(inner: &Inner, dest: &Path) -> Result<BackupSummary> {
    if dest.exists() {
        return Err(StorageError::Unavailable(format!(
            "{} already exists",
            dest.display()
        )));
    }
    let partial = partial_path(dest);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(e, parent))?;
    }
    fs::create_dir(&partial).map_err(|e| io_error(e, &partial))?;
    match copy_into(inner, &partial) {
        Ok(summary) => {
            fs::rename(&partial, dest).map_err(|e| io_error(e, dest))?;
            if let Some(parent) = dest.parent() {
                sync_dir(parent)?;
            }
            Ok(summary)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&partial);
            Err(e)
        }
    }
}

/// Copies the session log and then every topic into an empty directory.
///
/// Args:
///     inner: The open store.
///     root: The directory to fill.
///
/// Returns:
///     What was copied.
///
/// Raises:
///     StorageError::Unavailable: When a file can't be read, written or flushed.
///     StorageError::Other: When a session change can't be encoded.
fn copy_into(inner: &Inner, root: &Path) -> Result<BackupSummary> {
    let sessions = lock(&inner.sessions).snapshot();
    let sessions_path = root.join("sessions.log");
    write_compacted(&sessions_path, sessions)?;
    let mut summary = BackupSummary {
        bytes: fs::metadata(&sessions_path)
            .map_err(|e| io_error(e, &sessions_path))?
            .len(),
        ..BackupSummary::default()
    };
    let topics_dir = root.join("topics");
    fs::create_dir(&topics_dir).map_err(|e| io_error(e, &topics_dir))?;
    let topics: Vec<(String, Arc<_>)> = lock(&inner.topics)
        .iter()
        .map(|(name, topic)| (name.clone(), topic.clone()))
        .collect();
    for (name, topic) in topics {
        let snapshot = lock(&topic).snapshot()?;
        summary.topics += 1;
        summary.records += snapshot.records();
        summary.bytes += snapshot.write_to(&topics_dir.join(topic_dir_name(&name)))?;
    }
    sync_dir(&topics_dir)?;
    sync_dir(root)?;
    Ok(summary)
}
