//! `LogIndex`, the bookkeeping every topic log needs, whatever holds the records.
//!
//! A backend keeps one `LogIndex` per topic. It numbers records, finds
//! duplicates from the same publisher, keeps publish times from going
//! backwards, answers `bounds` and `seq_at`, and decides what retention
//! trims. Each entry carries a value of the backend's choosing: the whole
//! `Record` for `MemoryStorage`, where the record sits on disk for
//! `FileStorage`. Keeping these rules in one place is what makes the
//! backends agree; the conformance suite checks the result.

use std::collections::{HashMap, VecDeque};

use crate::record::{LogBounds, NewRecord};
use crate::storage::Retention;

/// The key a publisher retry is recognised by: publisher, then message id.
pub(crate) type RetryKey = (String, String);

/// One retained record as the index sees it.
#[derive(Debug, Clone)]
pub(crate) struct Entry<T> {
    /// The record's sequence number.
    pub seq: u64,
    /// The publish time the log settled on.
    pub published_at_ms: i64,
    /// The record's `Record::size`, counted against the byte limit.
    pub size: u64,
    /// The publisher and message id, for retry detection, or `None` when
    /// the record has no id of the publisher's. Boxed, so a record without
    /// one costs a pointer in memory rather than two empty strings.
    pub key: Option<Box<RetryKey>>,
    /// What the backend keeps for this record.
    pub value: T,
}

/// Where the next record goes, worked out before the backend stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slot {
    /// The `seq` the record gets.
    pub seq: u64,
    /// The publish time it is stored with, never earlier than the last one.
    pub published_at_ms: i64,
}

/// One topic's retained records and the counters that outlive them.
#[derive(Debug)]
pub(crate) struct LogIndex<T> {
    /// Retained records, oldest first, with consecutive `seq` values.
    entries: VecDeque<Entry<T>>,
    /// The highest `seq` ever appended, kept when records are trimmed.
    last_seq: u64,
    /// The highest publish time ever appended, kept when records are trimmed.
    last_published_at_ms: i64,
    /// The total `size` of `entries`.
    bytes: u64,
    /// Maps each retained record's retry key to its `seq`.
    ids: HashMap<RetryKey, u64, ahash::RandomState>,
    /// The publisher of `seq` 1, which created the topic, kept when records
    /// are trimmed. `None` for a topic created before creators were kept.
    creator: Option<String>,
}

impl<T> Default for LogIndex<T> {
    /// Builds an index for a topic that has never had a record.
    ///
    /// Returns:
    ///     An empty index.
    fn default() -> Self {
        Self::resume(0, 0)
    }
}

impl<T> LogIndex<T> {
    /// Builds an empty index that continues after records already numbered.
    ///
    /// A backend reopening a topic whose records were all trimmed uses this,
    /// so sequence numbers and publish times carry on instead of restarting.
    ///
    /// Args:
    ///     last_seq: The highest `seq` the topic ever had.
    ///     last_published_at_ms: The highest publish time it ever had.
    ///
    /// Returns:
    ///     An index with no retained records.
    pub(crate) fn resume(last_seq: u64, last_published_at_ms: i64) -> Self {
        Self {
            entries: VecDeque::new(),
            last_seq,
            last_published_at_ms,
            bytes: 0,
            ids: HashMap::default(),
            creator: None,
        }
    }

    /// The account whose publish created the topic.
    ///
    /// Returns:
    ///     The publisher of `seq` 1, or `None` when it isn't known.
    pub(crate) fn creator(&self) -> Option<&str> {
        self.creator.as_deref()
    }

    /// Records the account that created the topic. A backend calls it when
    /// `seq` 1 is appended, and when it reopens a topic whose creator it kept.
    ///
    /// Args:
    ///     creator: The publisher of `seq` 1.
    pub(crate) fn set_creator(&mut self, creator: Option<String>) {
        self.creator = creator;
    }

    /// Reports which sequence numbers are retained.
    ///
    /// Returns:
    ///     The bounds.
    pub(crate) fn bounds(&self) -> LogBounds {
        LogBounds {
            first_seq: self.entries.front().map_or(self.last_seq + 1, |e| e.seq),
            last_seq: self.last_seq,
        }
    }

    /// The highest publish time ever appended.
    ///
    /// Returns:
    ///     The time, in milliseconds since the Unix epoch.
    pub(crate) fn last_published_at_ms(&self) -> i64 {
        self.last_published_at_ms
    }

    /// Finds a retained record from the same publisher with the same message id.
    ///
    /// Args:
    ///     record: The record about to be appended.
    ///
    /// Returns:
    ///     The original entry, if there is one.
    pub(crate) fn duplicate_of(&self, record: &NewRecord) -> Option<&Entry<T>> {
        if record.message_id.is_empty() {
            return None;
        }
        let seq = *self
            .ids
            .get(&(record.publisher.clone(), record.message_id.clone()))?;
        self.get(seq)
    }

    /// Works out the `seq` and publish time for the next record.
    ///
    /// Args:
    ///     record: The record about to be appended.
    ///
    /// Returns:
    ///     Where it goes. Nothing changes until `push`.
    pub(crate) fn next_slot(&self, record: &NewRecord) -> Slot {
        Slot {
            seq: self.last_seq + 1,
            published_at_ms: record.published_at_ms.max(self.last_published_at_ms),
        }
    }

    /// Adds a record the backend has just stored.
    ///
    /// Args:
    ///     slot: The slot from `next_slot`.
    ///     size: The record's `Record::size`.
    ///     key: The record's retry key.
    ///     value: What the backend keeps for it.
    pub(crate) fn push(&mut self, slot: Slot, size: u64, key: RetryKey, value: T) {
        self.last_seq = slot.seq;
        self.last_published_at_ms = slot.published_at_ms;
        self.bytes += size;
        let key = if key.1.is_empty() {
            None
        } else {
            self.ids.insert(key.clone(), slot.seq);
            Some(Box::new(key))
        };
        self.entries.push_back(Entry {
            seq: slot.seq,
            published_at_ms: slot.published_at_ms,
            size,
            key,
            value,
        });
    }

    /// Takes back the newest records, for a backend whose write of them failed.
    ///
    /// Args:
    ///     count: How many of the newest records to remove.
    ///     last_seq: The log's last `seq` before they were pushed.
    ///     last_published_at_ms: The log's last publish time before they were pushed.
    pub(crate) fn rollback(&mut self, count: usize, last_seq: u64, last_published_at_ms: i64) {
        for _ in 0..count {
            let Some(entry) = self.entries.pop_back() else {
                break;
            };
            self.bytes -= entry.size;
            if let Some(key) = &entry.key
                && self.ids.get(key.as_ref()) == Some(&entry.seq)
            {
                self.ids.remove(key.as_ref());
            }
        }
        self.last_seq = last_seq;
        self.last_published_at_ms = last_published_at_ms;
    }

    /// Looks up a retained record by `seq`.
    ///
    /// Args:
    ///     seq: The sequence number.
    ///
    /// Returns:
    ///     The entry, or `None` if it isn't retained.
    pub(crate) fn get(&self, seq: u64) -> Option<&Entry<T>> {
        let first = self.entries.front()?.seq;
        let index = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.entries.get(index)
    }

    /// Lists up to `max` retained records starting at `from`.
    ///
    /// Args:
    ///     from: The first `seq` wanted; anything older starts at the oldest retained.
    ///     max: The most entries to return.
    ///
    /// Returns:
    ///     The entries in `seq` order.
    pub(crate) fn range(&self, from: u64, max: usize) -> impl Iterator<Item = &Entry<T>> {
        let first = self.bounds().first_seq;
        let skip = usize::try_from(from.saturating_sub(first)).unwrap_or(usize::MAX);
        self.entries.iter().skip(skip).take(max)
    }

    /// Finds the first retained record published at or after a time.
    ///
    /// Args:
    ///     time_ms: The time.
    ///
    /// Returns:
    ///     Its `seq`, or `None` when every retained record is older.
    pub(crate) fn seq_at(&self, time_ms: i64) -> Option<u64> {
        let index = self
            .entries
            .partition_point(|e| e.published_at_ms < time_ms);
        self.entries.get(index).map(|e| e.seq)
    }

    /// Removes records from the front while `Retention::must_trim` says so.
    ///
    /// Args:
    ///     policy: The limits to apply.
    ///     now_ms: The current time.
    ///
    /// Returns:
    ///     The removed entries, oldest first.
    pub(crate) fn trim(&mut self, policy: &Retention, now_ms: i64) -> Vec<Entry<T>> {
        let mut removed = Vec::new();
        while let Some(oldest) = self.entries.front()
            && policy.must_trim(
                self.entries.len() as u64,
                self.bytes,
                oldest.published_at_ms,
                now_ms,
            )
            && let Some(entry) = self.entries.pop_front()
        {
            self.bytes -= entry.size;
            if let Some(key) = &entry.key
                && self.ids.get(key.as_ref()) == Some(&entry.seq)
            {
                self.ids.remove(key.as_ref());
            }
            removed.push(entry);
        }
        removed
    }
}
