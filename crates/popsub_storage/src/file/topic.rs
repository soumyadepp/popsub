//! One topic's directory in `FileStorage`: segment files, the head file, and
//! the index built from them.
//!
//! Records are appended as frames (see `codec`) to the newest segment file.
//! Segments are named by the `seq` of their first record and roll over once
//! they reach the segment size, so trimming deletes whole files from the
//! front. Because a segment can be partly trimmed, `head.json` records the
//! first retained `seq`, plus the last `seq` and publish time so numbering
//! carries on even after everything was trimmed. It is replaced atomically
//! (write a temporary file, then rename) before any segment is deleted.
//!
//! Opening scans every segment and rebuilds the `LogIndex` in memory, with
//! each record's location. A bad frame at the end of the newest segment is
//! an interrupted write: the file is truncated there and the topic opens.
//! A bad frame anywhere else, a frame that doesn't decode, or a gap in the
//! sequence is corruption, and opening fails with the file and offset.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, StorageError};
use crate::file::codec::{self, Decoded, HEADER, MAX_FRAME};
use crate::file::{io_error, sync_dir};
use crate::index::LogIndex;
use crate::record::{LogBounds, NewRecord, Record};
use crate::storage::{Durability, Retention};

/// The name of the file that records a topic's trimmed prefix and counters.
const HEAD_FILE: &str = "head.json";

/// Where a record sits on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Location {
    /// The first `seq` of the segment holding the record, which names the file.
    segment: u64,
    /// The byte offset of the record's frame in that file.
    offset: u64,
}

/// The contents of `head.json`.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct Head {
    /// The oldest retained `seq`. Records below it were trimmed, even if
    /// their segment file still exists.
    first_seq: u64,
    /// The highest `seq` ever appended when the head was written.
    last_seq: u64,
    /// The highest publish time ever appended when the head was written.
    last_published_at_ms: i64,
}

/// One segment file.
#[derive(Debug, Clone, Copy)]
struct Segment {
    /// The `seq` of its first record, and its file name.
    first_seq: u64,
    /// Its length in bytes.
    len: u64,
}

/// A topic's files and the index of its retained records.
#[derive(Debug)]
pub(crate) struct TopicFiles {
    /// The topic name, checked against every record read back.
    topic: String,
    /// The topic's directory.
    dir: PathBuf,
    /// Segment files, oldest first. The last one is appended to.
    segments: Vec<Segment>,
    /// An append handle on the last segment, opened on first use.
    active: Option<File>,
    /// Retained records and where each one is.
    index: LogIndex<Location>,
    /// The size at which a new segment is started.
    segment_bytes: u64,
}

/// Builds the path of a segment file.
///
/// Args:
///     dir: The topic's directory.
///     first_seq: The segment's first `seq`.
///
/// Returns:
///     `<dir>/<first_seq, 20 digits>.seg`, so names sort in `seq` order.
fn segment_path(dir: &Path, first_seq: u64) -> PathBuf {
    dir.join(format!("{first_seq:020}.seg"))
}

impl TopicFiles {
    /// Opens a topic's directory, creating it if needed, and rebuilds its index.
    ///
    /// Args:
    ///     dir: The topic's directory.
    ///     topic: The topic name.
    ///     segment_bytes: The size at which a new segment is started.
    ///
    /// Returns:
    ///     The opened topic.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the directory or a file can't be
    ///         read, created or truncated.
    ///     StorageError::Corrupt: When a segment is damaged anywhere but at
    ///         the end of the newest one, or `head.json` can't be parsed.
    pub(crate) fn open(dir: &Path, topic: &str, segment_bytes: u64) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| io_error(e, dir))?;
        let head = read_head(dir)?;
        let mut first_seqs = list_segments(dir)?;
        let dropped = first_seqs
            .windows(2)
            .take_while(|pair| pair[1] <= head.first_seq)
            .count();
        for first_seq in first_seqs.drain(..dropped) {
            let path = segment_path(dir, first_seq);
            fs::remove_file(&path).map_err(|e| io_error(e, &path))?;
        }

        let mut index: LogIndex<Location> = LogIndex::default();
        let mut segments = Vec::with_capacity(first_seqs.len());
        let mut expected: Option<u64> = None;
        let mut scanned_last = (0, 0);
        let newest = first_seqs.len().saturating_sub(1);
        for (position, &first_seq) in first_seqs.iter().enumerate() {
            let path = segment_path(dir, first_seq);
            let bytes = fs::read(&path).map_err(|e| io_error(e, &path))?;
            let mut offset = 0;
            if let Some(expected) = expected
                && expected != first_seq
            {
                return Err(StorageError::Corrupt(format!(
                    "{} starts at seq {first_seq} but the previous segment ends at {}",
                    path.display(),
                    expected - 1
                )));
            }
            let mut expected_here = first_seq;
            loop {
                match codec::decode(&bytes, offset) {
                    Decoded::End => break,
                    Decoded::Torn if position == newest => {
                        truncate(&path, offset as u64)?;
                        break;
                    }
                    Decoded::Torn => {
                        return Err(StorageError::Corrupt(format!(
                            "damaged record in {} at offset {offset}",
                            path.display()
                        )));
                    }
                    Decoded::Frame { body, next } => {
                        let record: Record = serde_json::from_slice(body).map_err(|e| {
                            StorageError::Corrupt(format!(
                                "undecodable record in {} at offset {offset}: {e}",
                                path.display()
                            ))
                        })?;
                        if record.seq != expected_here || record.topic != topic {
                            return Err(StorageError::Corrupt(format!(
                                "unexpected record (topic {:?}, seq {}) in {} at offset {offset}",
                                record.topic,
                                record.seq,
                                path.display()
                            )));
                        }
                        expected_here = record.seq + 1;
                        scanned_last = (record.seq, record.published_at_ms);
                        if record.seq >= head.first_seq {
                            let slot = crate::index::Slot {
                                seq: record.seq,
                                published_at_ms: record.published_at_ms,
                            };
                            let location = Location {
                                segment: first_seq,
                                offset: offset as u64,
                            };
                            let size = record.size();
                            index.push(slot, size, (record.publisher, record.message_id), location);
                        }
                        offset = next;
                    }
                }
            }
            expected = Some(expected_here);
            let len = fs::metadata(&path).map_err(|e| io_error(e, &path))?.len();
            segments.push(Segment { first_seq, len });
        }
        if index.bounds().is_empty() {
            index = LogIndex::resume(
                head.last_seq.max(scanned_last.0),
                head.last_published_at_ms.max(scanned_last.1),
            );
        }
        Ok(Self {
            topic: topic.to_string(),
            dir: dir.to_path_buf(),
            segments,
            active: None,
            index,
            segment_bytes,
        })
    }

    /// Reports which sequence numbers are retained.
    ///
    /// Returns:
    ///     The bounds.
    pub(crate) fn bounds(&self) -> LogBounds {
        self.index.bounds()
    }

    /// Finds the first retained record published at or after a time.
    ///
    /// Args:
    ///     time_ms: The time.
    ///
    /// Returns:
    ///     Its `seq`, or `None`.
    pub(crate) fn seq_at(&self, time_ms: i64) -> Option<u64> {
        self.index.seq_at(time_ms)
    }

    /// Appends a record, or returns the original for a retried one.
    ///
    /// The frame is written to the newest segment, starting a new one first
    /// if it is full. With `Durability::Synced` the file is flushed to disk
    /// before returning (and the directory too when a segment was created);
    /// otherwise the write has reached the operating system, which survives
    /// the process crashing. If the write fails, the file is cut back to its
    /// previous length so no partial frame is left.
    ///
    /// Args:
    ///     record: The record to append.
    ///     durability: How firmly to store it.
    ///
    /// Returns:
    ///     The stored record.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When a file can't be created, written or synced.
    ///     StorageError::Corrupt: When the original of a retried record can't be read back.
    pub(crate) fn append(&mut self, record: NewRecord, durability: Durability) -> Result<Record> {
        if let Some(original) = self.index.duplicate_of(&record) {
            let location = original.value;
            return self.read_at(&mut HashMap::new(), location);
        }
        let slot = self.index.next_slot(&record);
        let key = (record.publisher.clone(), record.message_id.clone());
        let stored = Record::from_new(&self.topic, slot.seq, record, slot.published_at_ms);
        let body = serde_json::to_vec(&stored)
            .map_err(|e| StorageError::Other(format!("encoding a record: {e}")))?;
        let frame = codec::encode(&body);
        self.ensure_segment(slot.seq, durability)?;
        let segment = self
            .segments
            .last_mut()
            .expect("ensure_segment leaves a segment");
        let path = segment_path(&self.dir, segment.first_seq);
        let file = self
            .active
            .as_mut()
            .expect("ensure_segment opens the active segment");
        let offset = segment.len;
        if let Err(e) = file.write_all(&frame) {
            let _ = file.set_len(offset);
            return Err(io_error(e, &path));
        }
        if durability == Durability::Synced {
            file.sync_data().map_err(|e| io_error(e, &path))?;
        }
        segment.len += frame.len() as u64;
        let location = Location {
            segment: segment.first_seq,
            offset,
        };
        self.index.push(slot, stored.size(), key, location);
        Ok(stored)
    }

    /// Makes sure there is a segment with room to append to, and an open handle on it.
    ///
    /// Args:
    ///     next_seq: The `seq` of the record about to be written, which names
    ///         a new segment.
    ///     durability: When `Synced`, the directory is synced after creating
    ///         a segment so the new file survives a power loss.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the segment can't be created or opened.
    fn ensure_segment(&mut self, next_seq: u64, durability: Durability) -> Result<()> {
        let full = self
            .segments
            .last()
            .is_none_or(|s| s.len > 0 && s.len >= self.segment_bytes);
        if full {
            let path = segment_path(&self.dir, next_seq);
            let file = OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path)
                .map_err(|e| io_error(e, &path))?;
            if durability == Durability::Synced {
                sync_dir(&self.dir)?;
            }
            self.segments.push(Segment {
                first_seq: next_seq,
                len: 0,
            });
            self.active = Some(file);
        } else if self.active.is_none() {
            let segment = self
                .segments
                .last()
                .expect("not full means a segment exists");
            let path = segment_path(&self.dir, segment.first_seq);
            let file = OpenOptions::new()
                .append(true)
                .open(&path)
                .map_err(|e| io_error(e, &path))?;
            self.active = Some(file);
        }
        Ok(())
    }

    /// Reads up to `max` retained records starting at `from`.
    ///
    /// Args:
    ///     from: The first `seq` wanted.
    ///     max: The most records to return.
    ///
    /// Returns:
    ///     The records in `seq` order.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When a segment can't be opened or read.
    ///     StorageError::Corrupt: When a record fails its CRC or doesn't decode.
    pub(crate) fn read(&self, from: u64, max: usize) -> Result<Vec<Record>> {
        let locations: Vec<Location> = self.index.range(from, max).map(|e| e.value).collect();
        let mut files = HashMap::new();
        locations
            .into_iter()
            .map(|location| self.read_at(&mut files, location))
            .collect()
    }

    /// Reads one record from disk and checks it.
    ///
    /// Args:
    ///     files: Segment handles already opened during this call, by first `seq`.
    ///     location: Where the record is.
    ///
    /// Returns:
    ///     The record.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the segment can't be opened or read.
    ///     StorageError::Corrupt: When the frame fails its CRC or doesn't decode.
    fn read_at(&self, files: &mut HashMap<u64, File>, location: Location) -> Result<Record> {
        let path = segment_path(&self.dir, location.segment);
        let file = match files.entry(location.segment) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(File::open(&path).map_err(|e| io_error(e, &path))?)
            }
        };
        let corrupt = |what: &str| {
            StorageError::Corrupt(format!(
                "{what} in {} at offset {}",
                path.display(),
                location.offset
            ))
        };
        file.seek(SeekFrom::Start(location.offset))
            .map_err(|e| io_error(e, &path))?;
        let mut header = [0u8; HEADER];
        file.read_exact(&mut header)
            .map_err(|e| io_error(e, &path))?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if len > MAX_FRAME {
            return Err(corrupt("oversized frame"));
        }
        let mut frame = header.to_vec();
        frame.resize(HEADER + len, 0);
        file.read_exact(&mut frame[HEADER..])
            .map_err(|e| io_error(e, &path))?;
        let Decoded::Frame { body, .. } = codec::decode(&frame, 0) else {
            return Err(corrupt("checksum mismatch"));
        };
        serde_json::from_slice(body).map_err(|_| corrupt("undecodable record"))
    }

    /// Trims records outside the policy and deletes segments left with none.
    ///
    /// The head file is rewritten before any segment is deleted, so a crash
    /// in between leaves extra files that the next open deletes, never
    /// trimmed records coming back.
    ///
    /// Args:
    ///     policy: The limits to apply.
    ///     now_ms: The current time.
    ///
    /// Returns:
    ///     How many records were removed.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the head can't be written or a
    ///         segment can't be deleted. Records already dropped from the
    ///         index stay dropped; on a restart before the head is written
    ///         they come back and are trimmed again.
    pub(crate) fn trim(&mut self, policy: &Retention, now_ms: i64) -> Result<u64> {
        let removed = self.index.trim(policy, now_ms).len() as u64;
        if removed == 0 {
            return Ok(0);
        }
        let bounds = self.index.bounds();
        write_head(
            &self.dir,
            Head {
                first_seq: bounds.first_seq,
                last_seq: bounds.last_seq,
                last_published_at_ms: self.index.last_published_at_ms(),
            },
        )?;
        while self.segments.len() > 1 && self.segments[1].first_seq <= bounds.first_seq {
            let path = segment_path(&self.dir, self.segments[0].first_seq);
            fs::remove_file(&path).map_err(|e| io_error(e, &path))?;
            self.segments.remove(0);
        }
        Ok(removed)
    }
}

/// Lists the segment files in a topic directory.
///
/// Args:
///     dir: The topic's directory.
///
/// Returns:
///     The first `seq` of each segment, sorted.
///
/// Raises:
///     StorageError::Unavailable: When the directory can't be listed.
fn list_segments(dir: &Path) -> Result<Vec<u64>> {
    let mut first_seqs = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| io_error(e, dir))? {
        let path = entry.map_err(|e| io_error(e, dir))?.path();
        if path.extension().is_some_and(|ext| ext == "seg")
            && let Some(first_seq) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())
        {
            first_seqs.push(first_seq);
        }
    }
    first_seqs.sort_unstable();
    Ok(first_seqs)
}

/// Cuts a file back to a length, dropping an interrupted write at its end.
///
/// Args:
///     path: The file.
///     len: The length to keep.
///
/// Raises:
///     StorageError::Unavailable: When the file can't be opened or truncated.
fn truncate(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| io_error(e, path))?;
    file.set_len(len).map_err(|e| io_error(e, path))?;
    file.sync_all().map_err(|e| io_error(e, path))
}

/// Reads `head.json`, or the default head if there is none.
///
/// Args:
///     dir: The topic's directory.
///
/// Returns:
///     The head.
///
/// Raises:
///     StorageError::Unavailable: When the file exists but can't be read.
///     StorageError::Corrupt: When it can't be parsed.
fn read_head(dir: &Path) -> Result<Head> {
    let path = dir.join(HEAD_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| StorageError::Corrupt(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Head::default()),
        Err(e) => Err(io_error(e, &path)),
    }
}

/// Replaces `head.json` atomically: write a temporary file, sync it, rename it.
///
/// Args:
///     dir: The topic's directory.
///     head: The new head.
///
/// Raises:
///     StorageError::Unavailable: When the file can't be written, synced or renamed.
fn write_head(dir: &Path, head: Head) -> Result<()> {
    let path = dir.join(HEAD_FILE);
    let temp = dir.join(format!("{HEAD_FILE}.tmp"));
    let bytes = serde_json::to_vec(&head)
        .map_err(|e| StorageError::Other(format!("encoding a head: {e}")))?;
    let mut file = File::create(&temp).map_err(|e| io_error(e, &temp))?;
    file.write_all(&bytes).map_err(|e| io_error(e, &temp))?;
    file.sync_all().map_err(|e| io_error(e, &temp))?;
    fs::rename(&temp, &path).map_err(|e| io_error(e, &path))?;
    sync_dir(dir)
}
