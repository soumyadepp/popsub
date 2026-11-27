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
use crate::file::body;
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

/// The largest buffer a topic keeps between batches. A batch of unusually
/// large records gives its buffer back to the allocator instead.
const KEPT_BUFFER_BYTES: usize = 1024 * 1024;

/// Frames of one batch that are waiting to be written to the active segment
/// with a single write, and what to undo if that write fails.
#[derive(Debug, Default)]
struct Run {
    /// The encoded frames, back to back.
    bytes: Vec<u8>,
    /// The positions, in the batch's results, of the records framed here.
    results: Vec<usize>,
    /// The active segment's length before the first frame.
    start_len: u64,
    /// The log's last `seq` before the first frame.
    last_seq: u64,
    /// The log's last publish time before the first frame.
    last_published_at_ms: i64,
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
    /// The buffer the last batch's frames were written from, kept so the
    /// next batch doesn't allocate and free one of its own.
    frames: Vec<u8>,
    /// The buffer each record's stored form is written to before it is framed.
    scratch: Vec<u8>,
    /// Retained records and where each one is.
    index: LogIndex<Location>,
    /// The size at which a new segment is started.
    segment_bytes: u64,
    /// Whether records were trimmed since `head.json` was last written.
    head_stale: bool,
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
                        let record: Record = body::decode(body, topic).map_err(|e| {
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
            frames: Vec::new(),
            scratch: Vec::new(),
            head_stale: false,
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

    /// Appends one record. See `append_batch`, which this is a batch of one of.
    ///
    /// Args:
    ///     record: The record to append.
    ///     durability: How durable the append must be before this returns.
    ///
    /// Returns:
    ///     The record as stored, or the original when it is a retry.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the segment can't be written or read.
    ///     StorageError::Corrupt: When a retry's original can't be decoded.
    pub(crate) fn append(&mut self, record: NewRecord, durability: Durability) -> Result<Record> {
        self.append_batch(vec![record], durability)
            .pop()
            .expect("one result per record")
    }

    /// Appends records in order, writing the frames that share a segment with one write.
    ///
    /// A record that repeats a retained `(publisher, message id)` isn't
    /// stored again: its result is the original record. Each new record is
    /// framed into a buffer and indexed, and the buffer is written to the
    /// active segment once, at the end or when the segment is full. With
    /// `Durability::Synced` each write is followed by one sync, so a batch
    /// costs one sync however many records it holds.
    ///
    /// If a write or sync fails, the file is cut back to where the write
    /// began, the records in it leave the index, and their results are the
    /// error. Records written by an earlier write of the same batch stay.
    ///
    /// Args:
    ///     records: The records to append, oldest first.
    ///     durability: How durable the appends must be before this returns.
    ///
    /// Returns:
    ///     One result per record, in the same order.
    pub(crate) fn append_batch(
        &mut self,
        records: Vec<NewRecord>,
        durability: Durability,
    ) -> Vec<Result<Record>> {
        let mut results: Vec<Result<Record>> = Vec::with_capacity(records.len());
        let mut run = Run {
            bytes: std::mem::take(&mut self.frames),
            ..Run::default()
        };
        let mut scratch = std::mem::take(&mut self.scratch);
        for record in records {
            if let Some(original) = self.index.duplicate_of(&record) {
                let location = original.value;
                self.write_run(&mut run, durability, &mut results);
                results.push(self.read_at(&mut HashMap::new(), location));
                continue;
            }
            let full = self
                .segments
                .last()
                .is_none_or(|s| s.len > 0 && s.len >= self.segment_bytes);
            if full {
                self.write_run(&mut run, durability, &mut results);
            }
            let slot = self.index.next_slot(&record);
            if let Err(e) = self.ensure_segment(slot.seq, durability) {
                results.push(Err(e));
                continue;
            }
            let key = if record.message_id.is_empty() {
                (String::new(), String::new())
            } else {
                (record.publisher.clone(), record.message_id.clone())
            };
            let stored = Record::from_new(&self.topic, slot.seq, record, slot.published_at_ms);
            scratch.clear();
            body::encode(&stored, &mut scratch);
            let segment = self
                .segments
                .last_mut()
                .expect("ensure_segment leaves a segment");
            if run.results.is_empty() {
                run.start_len = segment.len;
                run.last_seq = self.index.bounds().last_seq;
                run.last_published_at_ms = self.index.last_published_at_ms();
            }
            let location = Location {
                segment: segment.first_seq,
                offset: segment.len,
            };
            segment.len += codec::encode_into(&mut run.bytes, &scratch) as u64;
            run.results.push(results.len());
            self.index.push(slot, stored.size(), key, location);
            results.push(Ok(stored));
        }
        self.write_run(&mut run, durability, &mut results);
        if run.bytes.capacity() <= KEPT_BUFFER_BYTES {
            self.frames = run.bytes;
        }
        if scratch.capacity() <= KEPT_BUFFER_BYTES {
            scratch.clear();
            self.scratch = scratch;
        }
        results
    }

    /// Writes a run's buffered frames to the active segment, and empties the run.
    ///
    /// On failure the segment is cut back to its length before the run, the
    /// run's records are taken out of the index, and their results become
    /// the error.
    ///
    /// Args:
    ///     run: The frames waiting to be written, and which results they belong to.
    ///     durability: Whether to sync the segment after writing.
    ///     results: The batch's results so far.
    fn write_run(&mut self, run: &mut Run, durability: Durability, results: &mut [Result<Record>]) {
        if run.results.is_empty() {
            return;
        }
        if let Some(segment) = self.segments.last_mut()
            && let Some(file) = self.active.as_mut()
        {
            let written = file.write_all(&run.bytes).and_then(|()| {
                if durability == Durability::Synced {
                    file.sync_data()
                } else {
                    Ok(())
                }
            });
            if let Err(e) = written {
                let _ = file.set_len(run.start_len);
                segment.len = run.start_len;
                self.index
                    .rollback(run.results.len(), run.last_seq, run.last_published_at_ms);
                let error = io_error(e, &segment_path(&self.dir, segment.first_seq));
                for &index in &run.results {
                    results[index] = Err(error.clone());
                }
            }
        }
        run.bytes.clear();
        run.results.clear();
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
        let Some(newest) = self.index.range(from, max).last() else {
            return Ok(Vec::new());
        };
        let after = self.index.get(newest.seq + 1).map(|entry| entry.value);
        let mut records = Vec::with_capacity(locations.len());
        let mut start = 0;
        while start < locations.len() {
            let segment = locations[start].segment;
            let count = locations[start..]
                .iter()
                .take_while(|location| location.segment == segment)
                .count();
            let next = locations.get(start + count).copied().or(after);
            self.read_run(&locations[start..start + count], next, &mut records)?;
            start += count;
        }
        Ok(records)
    }

    /// Reads records that sit back to back in one segment with a single read.
    ///
    /// Args:
    ///     run: Where each record's frame starts, in `seq` order, all in the
    ///         same segment and with no retained record between them.
    ///     next: Where the record after the run starts, if there is one.
    ///     records: Gains the run's records, in order.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the segment can't be opened or read.
    ///     StorageError::Corrupt: When a frame isn't where the index says, or
    ///         fails its CRC or doesn't decode.
    fn read_run(
        &self,
        run: &[Location],
        next: Option<Location>,
        records: &mut Vec<Record>,
    ) -> Result<()> {
        let Some(&first) = run.first() else {
            return Ok(());
        };
        let path = segment_path(&self.dir, first.segment);
        let end = match next {
            Some(next) if next.segment == first.segment => next.offset,
            _ => self
                .segments
                .iter()
                .find(|segment| segment.first_seq == first.segment)
                .map_or(first.offset, |segment| segment.len),
        };
        let corrupt = |what: &str, offset: u64| {
            StorageError::Corrupt(format!("{what} in {} at offset {offset}", path.display()))
        };
        let len = usize::try_from(end.saturating_sub(first.offset))
            .map_err(|_| corrupt("oversized run", first.offset))?;
        let mut file = File::open(&path).map_err(|e| io_error(e, &path))?;
        file.seek(SeekFrom::Start(first.offset))
            .map_err(|e| io_error(e, &path))?;
        let mut bytes = vec![0u8; len];
        file.read_exact(&mut bytes)
            .map_err(|e| io_error(e, &path))?;
        for location in run {
            let at = usize::try_from(location.offset - first.offset)
                .map_err(|_| corrupt("oversized run", location.offset))?;
            let Decoded::Frame { body, .. } = codec::decode(&bytes, at) else {
                return Err(corrupt("damaged frame", location.offset));
            };
            records.push(
                body::decode(body, &self.topic)
                    .map_err(|_| corrupt("undecodable record", location.offset))?,
            );
        }
        Ok(())
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
        body::decode(body, &self.topic).map_err(|_| corrupt("undecodable record"))
    }

    /// Drops records outside the policy from the index, without touching a file.
    ///
    /// Readers stop seeing the records at once. `head.json` is only worth
    /// rewriting when a whole segment can be deleted or the log is now
    /// empty, so a topic that trims one record on every publish doesn't
    /// write and sync a file each time. Until then the head is stale: after
    /// a restart, records trimmed since it was last written are indexed
    /// again and removed by the next trim.
    ///
    /// Args:
    ///     policy: The limits to apply.
    ///     now_ms: The current time.
    ///
    /// Returns:
    ///     How many records were removed, and whether `persist_trim` should
    ///     run now.
    pub(crate) fn trim_index(&mut self, policy: &Retention, now_ms: i64) -> (u64, bool) {
        let removed = self.index.trim(policy, now_ms).len() as u64;
        if removed == 0 {
            return (0, false);
        }
        self.head_stale = true;
        let bounds = self.index.bounds();
        let segment_is_empty =
            self.segments.len() > 1 && self.segments[1].first_seq <= bounds.first_seq;
        (removed, bounds.is_empty() || segment_is_empty)
    }

    /// Writes `head.json` and deletes the segments left with no records.
    ///
    /// The head file is rewritten before any segment is deleted, so a crash
    /// in between leaves extra files that the next open deletes.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the head can't be written or a
    ///         segment can't be deleted.
    pub(crate) fn persist_trim(&mut self) -> Result<()> {
        let bounds = self.index.bounds();
        write_head(
            &self.dir,
            Head {
                first_seq: bounds.first_seq,
                last_seq: bounds.last_seq,
                last_published_at_ms: self.index.last_published_at_ms(),
            },
        )?;
        self.head_stale = false;
        while self.segments.len() > 1 && self.segments[1].first_seq <= bounds.first_seq {
            let path = segment_path(&self.dir, self.segments[0].first_seq);
            fs::remove_file(&path).map_err(|e| io_error(e, &path))?;
            self.segments.remove(0);
        }
        Ok(())
    }
}

impl Drop for TopicFiles {
    /// Writes a stale head when the store closes, so a clean restart starts
    /// from the first retained record. A failure is ignored: the next open
    /// indexes the trimmed records again and the next trim removes them.
    fn drop(&mut self) {
        if self.head_stale {
            let _ = self.persist_trim();
        }
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
