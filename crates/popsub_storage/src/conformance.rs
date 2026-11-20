//! The conformance suite: checks every storage backend must pass.
//!
//! A backend describes how to open itself with a `Fixture`, then generates
//! one test per check with `conformance_tests!`. Opening the same fixture
//! twice must reach the same data when the backend is persistent, which is
//! how the suite checks that records and cursors survive a reopen. Checks
//! that need persistence or `Durability::Synced` are skipped, and pass, when
//! the backend's `Capabilities` say it doesn't offer them.
//!
//! Available in this crate's tests and, for other crates, behind the
//! `conformance` feature.
//!
//! Example:
//!
//! ```ignore
//! struct TempFileFixture(tempfile::TempDir);
//!
//! impl popsub_storage::conformance::Fixture for TempFileFixture {
//!     type Storage = MyFileStorage;
//!
//!     async fn open(&self) -> MyFileStorage {
//!         MyFileStorage::open(self.0.path()).await.unwrap()
//!     }
//! }
//!
//! popsub_storage::conformance_tests!(TempFileFixture(tempfile::tempdir().unwrap()));
//! ```

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use crate::error::StorageError;
use crate::record::{LogBounds, NewRecord};
use crate::storage::{Durability, MessageLog, Retention, Storage, SubscriptionStore};

/// Opens a backend for the conformance suite.
///
/// Each generated test builds a new fixture, so fixtures that use disk should
/// own a fresh temporary location.
pub trait Fixture: Send + Sync + 'static {
    /// The backend under test.
    type Storage: Storage + 'static;

    /// Opens the backend at this fixture's location.
    ///
    /// The suite drops the previous instance before opening again, so a
    /// backend that locks its location can reopen it. Panics are fine here;
    /// they fail the test.
    ///
    /// Returns:
    ///     An open backend.
    fn open(&self) -> impl Future<Output = Self::Storage> + Send;
}

/// Runs a check to completion on a multi-threaded runtime.
///
/// The generated tests call this instead of `#[tokio::test]`, so a crate
/// using the suite doesn't need tokio's macros. Panics if the runtime can't
/// be built.
///
/// Args:
///     check: The check to run.
pub fn block_on(check: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .build()
        .expect("tokio runtime for the conformance suite")
        .block_on(check);
}

/// Builds a record for the checks.
///
/// Args:
///     id: The message id, also used as the payload.
///     published_at_ms: The publish time.
///
/// Returns:
///     A QoS 1 record from publisher `alice`.
fn record(id: &str, published_at_ms: i64) -> NewRecord {
    NewRecord {
        message_id: id.to_string(),
        publisher: "alice".to_string(),
        payload: id.to_string(),
        encoding: None,
        qos: 1,
        published_at_ms,
    }
}

/// Appends records `m1` to `mN` to a topic, published at times 1 to N.
///
/// Args:
///     storage: The backend.
///     topic: The topic to append to.
///     count: How many records to append.
///
/// Returns:
///     The sequence numbers the backend assigned, in order.
async fn append_many(storage: &impl Storage, topic: &str, count: u64) -> Vec<u64> {
    let mut seqs = Vec::new();
    for n in 1..=count {
        let seq = storage
            .append(topic, record(&format!("m{n}"), n as i64), Durability::None)
            .await
            .expect("append")
            .seq;
        seqs.push(seq);
    }
    seqs
}

/// Reads every retained record's `seq` from a topic.
///
/// Args:
///     storage: The backend.
///     topic: The topic to read.
///
/// Returns:
///     The sequence numbers in the order returned.
async fn all_seqs(storage: &impl Storage, topic: &str) -> Vec<u64> {
    storage
        .read(topic, 0, usize::MAX)
        .await
        .expect("read")
        .iter()
        .map(|r| r.seq)
        .collect()
}

/// Checks that sequence numbers start at 1 and increase by one.
///
/// Args:
///     fixture: Opens the backend.
pub async fn seq_starts_at_one_and_increments<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    assert_eq!(append_many(&storage, "t", 5).await, vec![1, 2, 3, 4, 5]);
    let bounds = storage.bounds("t").await.expect("bounds");
    assert_eq!(
        bounds,
        LogBounds {
            first_seq: 1,
            last_seq: 5
        }
    );
}

/// Checks that trimming every record doesn't reset the sequence.
///
/// Args:
///     fixture: Opens the backend.
pub async fn seq_is_never_reused_after_trim<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 3).await;
    let everything = Retention {
        max_messages: Some(0),
        ..Retention::default()
    };
    assert_eq!(storage.trim("t", &everything, 10).await.expect("trim"), 3);
    let bounds = storage.bounds("t").await.expect("bounds");
    assert!(bounds.is_empty());
    assert_eq!(bounds.next_seq(), 4);
    let seq = storage
        .append("t", record("later", 20), Durability::None)
        .await
        .expect("append")
        .seq;
    assert_eq!(seq, 4);
}

/// Checks that `read` returns records in order and honours `from` and `max`.
///
/// Args:
///     fixture: Opens the backend.
pub async fn read_respects_from_and_max<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 10).await;
    let page = storage.read("t", 4, 3).await.expect("read");
    let seqs: Vec<u64> = page.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![4, 5, 6]);
    assert_eq!(page[0].message_id, "m4");
    assert_eq!(page[0].topic, "t");
    assert_eq!(page[0].publisher, "alice");
    assert_eq!(page[0].qos, 1);
    assert_eq!(all_seqs(&storage, "t").await, (1..=10).collect::<Vec<_>>());
}

/// Checks that reading past the end, or an unknown topic, returns nothing.
///
/// Args:
///     fixture: Opens the backend.
pub async fn read_past_end_or_unknown_topic_is_empty<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 2).await;
    assert!(storage.read("t", 3, 10).await.expect("read").is_empty());
    assert!(storage.read("nope", 1, 10).await.expect("read").is_empty());
    assert_eq!(
        storage.bounds("nope").await.expect("bounds"),
        LogBounds::EMPTY
    );
    assert_eq!(storage.seq_at("nope", 0).await.expect("seq_at"), None);
}

/// Checks that reading from a trimmed `seq` starts at the oldest retained one.
///
/// Args:
///     fixture: Opens the backend.
pub async fn read_from_trimmed_seq_starts_at_oldest<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 5).await;
    let keep_two = Retention {
        max_messages: Some(2),
        ..Retention::default()
    };
    storage.trim("t", &keep_two, 10).await.expect("trim");
    assert_eq!(all_seqs(&storage, "t").await, vec![4, 5]);
    let first = storage.read("t", 1, 1).await.expect("read");
    assert_eq!(first[0].seq, 4);
}

/// Checks that the count limit trims the oldest records only.
///
/// Args:
///     fixture: Opens the backend.
pub async fn trim_by_count_keeps_newest<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 10).await;
    let keep_three = Retention {
        max_messages: Some(3),
        ..Retention::default()
    };
    assert_eq!(storage.trim("t", &keep_three, 100).await.expect("trim"), 7);
    assert_eq!(all_seqs(&storage, "t").await, vec![8, 9, 10]);
    assert_eq!(storage.trim("t", &keep_three, 100).await.expect("trim"), 0);
}

/// Checks that the age limit trims records older than the limit only.
///
/// Args:
///     fixture: Opens the backend.
pub async fn trim_by_age_keeps_recent<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 10).await;
    let five_ms = Retention {
        max_age_ms: Some(5),
        ..Retention::default()
    };
    assert_eq!(storage.trim("t", &five_ms, 10).await.expect("trim"), 4);
    assert_eq!(all_seqs(&storage, "t").await, vec![5, 6, 7, 8, 9, 10]);
}

/// Checks that the byte limit trims until the retained records fit.
///
/// Args:
///     fixture: Opens the backend.
pub async fn trim_by_bytes_keeps_what_fits<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "t", 4).await;
    let records = storage.read("t", 1, 4).await.expect("read");
    let last_two: u64 = records[2..].iter().map(|r| r.size()).sum();
    let fits_two = Retention {
        max_bytes: Some(last_two),
        ..Retention::default()
    };
    assert_eq!(storage.trim("t", &fits_two, 10).await.expect("trim"), 2);
    assert_eq!(all_seqs(&storage, "t").await, vec![3, 4]);
}

/// Checks that appending the same publisher and message id twice returns the
/// same `seq` and stores one record, while another publisher's identical id
/// is a new record.
///
/// Args:
///     fixture: Opens the backend.
pub async fn duplicate_message_id_returns_original_seq<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    let first = storage
        .append("t", record("same", 1), Durability::None)
        .await
        .expect("append")
        .seq;
    let again = storage
        .append("t", record("same", 2), Durability::None)
        .await
        .expect("append")
        .seq;
    assert_eq!(first, again);
    let original = storage.read("t", first, 1).await.expect("read");
    assert_eq!(original[0].published_at_ms, 1);
    let mut from_bob = record("same", 3);
    from_bob.publisher = "bob".to_string();
    let other = storage
        .append("t", from_bob, Durability::None)
        .await
        .expect("append")
        .seq;
    assert_eq!(other, first + 1);
    let elsewhere = storage
        .append("u", record("same", 4), Durability::None)
        .await
        .expect("append")
        .seq;
    assert_eq!(elsewhere, 1);
    assert_eq!(all_seqs(&storage, "t").await, vec![1, 2]);
}

/// Checks that publish times never decrease along a log, that `append`
/// returns the time actually stored, and that `seq_at` finds the first
/// record at or after a time.
///
/// Args:
///     fixture: Opens the backend.
pub async fn publish_times_never_decrease_and_seq_at_finds_them<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    let mut returned = Vec::new();
    for (id, at) in [("a", 100), ("b", 200), ("c", 150), ("d", 300)] {
        let stored = storage
            .append("t", record(id, at), Durability::None)
            .await
            .expect("append");
        returned.push(stored.published_at_ms);
    }
    assert_eq!(returned, vec![100, 200, 200, 300]);
    let times: Vec<i64> = storage
        .read("t", 1, 10)
        .await
        .expect("read")
        .iter()
        .map(|r| r.published_at_ms)
        .collect();
    assert_eq!(times, vec![100, 200, 200, 300]);
    assert_eq!(storage.seq_at("t", 0).await.expect("seq_at"), Some(1));
    assert_eq!(storage.seq_at("t", 150).await.expect("seq_at"), Some(2));
    assert_eq!(storage.seq_at("t", 201).await.expect("seq_at"), Some(4));
    assert_eq!(storage.seq_at("t", 301).await.expect("seq_at"), None);
}

/// Checks that `topics` lists every log, including one trimmed to empty.
///
/// Args:
///     fixture: Opens the backend.
pub async fn topics_lists_every_log<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "b", 1).await;
    append_many(&storage, "a", 1).await;
    let everything = Retention {
        max_messages: Some(0),
        ..Retention::default()
    };
    storage.trim("a", &everything, 10).await.expect("trim");
    assert_eq!(storage.topics().await.expect("topics"), vec!["a", "b"]);
}

/// Checks that appends to one topic don't change another topic's log.
///
/// Args:
///     fixture: Opens the backend.
pub async fn topics_are_independent<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    append_many(&storage, "a", 3).await;
    append_many(&storage, "b", 2).await;
    assert_eq!(all_seqs(&storage, "a").await, vec![1, 2, 3]);
    assert_eq!(all_seqs(&storage, "b").await, vec![1, 2]);
    let keep_none = Retention {
        max_messages: Some(0),
        ..Retention::default()
    };
    storage.trim("a", &keep_none, 10).await.expect("trim");
    assert_eq!(all_seqs(&storage, "b").await, vec![1, 2]);
}

/// Checks that concurrent appends to one topic form one gap-free sequence
/// with every record stored once.
///
/// Args:
///     fixture: Opens the backend.
pub async fn concurrent_appends_are_gap_free<F: Fixture>(fixture: F) {
    let storage = Arc::new(fixture.open().await);
    let mut tasks = Vec::new();
    for writer in 0..8 {
        let storage = Arc::clone(&storage);
        tasks.push(tokio::spawn(async move {
            for n in 0..25 {
                storage
                    .append("t", record(&format!("w{writer}-{n}"), 1), Durability::None)
                    .await
                    .expect("append");
            }
        }));
    }
    for task in tasks {
        task.await.expect("writer task");
    }
    let records = storage.read("t", 1, usize::MAX).await.expect("read");
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, (1..=200).collect::<Vec<_>>());
    let ids: HashSet<&str> = records.iter().map(|r| r.message_id.as_str()).collect();
    assert_eq!(ids.len(), 200);
}

/// Checks that a durability level above the backend's maximum is refused and
/// stores nothing. Passes trivially for backends that support `Synced`.
///
/// Args:
///     fixture: Opens the backend.
pub async fn unsupported_durability_is_refused<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    if storage.capabilities().max_durability == Durability::Synced {
        return;
    }
    let result = storage
        .append("t", record("m", 1), Durability::Synced)
        .await;
    assert!(matches!(result, Err(StorageError::Unsupported(_))));
    assert!(storage.bounds("t").await.expect("bounds").is_empty());
}

/// Checks that cursors start where subscribed and only move forward.
///
/// Args:
///     fixture: Opens the backend.
pub async fn cursors_only_move_forward<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    storage.subscribe("s", "t", 3).await.expect("subscribe");
    storage.ack("s", "t", 7).await.expect("ack");
    storage.ack("s", "t", 5).await.expect("ack");
    let subs = storage.load("s").await.expect("load");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].session_id, "s");
    assert_eq!(subs[0].topic, "t");
    assert_eq!(subs[0].acked_seq, 7);
}

/// Checks that acknowledging without a subscription creates nothing.
///
/// Args:
///     fixture: Opens the backend.
pub async fn ack_without_subscription_is_ignored<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    storage.ack("ghost", "t", 5).await.expect("ack");
    assert!(storage.load("ghost").await.expect("load").is_empty());
    storage.subscribe("s", "a", 0).await.expect("subscribe");
    storage.ack("s", "b", 5).await.expect("ack");
    let topics: Vec<String> = storage
        .load("s")
        .await
        .expect("load")
        .into_iter()
        .map(|s| s.topic)
        .collect();
    assert_eq!(topics, vec!["a"]);
}

/// Checks that subscribing again replaces the cursor and unsubscribing
/// removes only that topic.
///
/// Args:
///     fixture: Opens the backend.
pub async fn subscribe_replaces_and_unsubscribe_removes<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    storage.subscribe("s", "a", 10).await.expect("subscribe");
    storage.subscribe("s", "b", 0).await.expect("subscribe");
    storage.subscribe("s", "a", 2).await.expect("subscribe");
    storage.unsubscribe("s", "b").await.expect("unsubscribe");
    storage
        .unsubscribe("s", "never")
        .await
        .expect("unsubscribe");
    let subs = storage.load("s").await.expect("load");
    assert_eq!(subs.len(), 1);
    assert_eq!((subs[0].topic.as_str(), subs[0].acked_seq), ("a", 2));
}

/// Checks that sessions belong to one id and `load` sorts by topic.
///
/// Args:
///     fixture: Opens the backend.
pub async fn sessions_are_separate_and_sorted<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    storage.subscribe("s1", "z", 1).await.expect("subscribe");
    storage.subscribe("s1", "a", 2).await.expect("subscribe");
    storage.subscribe("s2", "a", 9).await.expect("subscribe");
    let s1: Vec<(String, u64)> = storage
        .load("s1")
        .await
        .expect("load")
        .into_iter()
        .map(|s| (s.topic, s.acked_seq))
        .collect();
    assert_eq!(s1, vec![("a".to_string(), 2), ("z".to_string(), 1)]);
    assert_eq!(storage.load("s2").await.expect("load")[0].acked_seq, 9);
    assert!(storage.load("s3").await.expect("load").is_empty());
}

/// Checks that `expire` removes idle sessions with their subscriptions and
/// keeps active ones.
///
/// Args:
///     fixture: Opens the backend.
pub async fn expire_removes_idle_sessions<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    storage.subscribe("old", "t", 1).await.expect("subscribe");
    storage.touch("old", 100).await.expect("touch");
    storage.subscribe("new", "t", 1).await.expect("subscribe");
    storage.touch("new", 500).await.expect("touch");
    storage.subscribe("never", "t", 1).await.expect("subscribe");
    assert_eq!(storage.expire(300).await.expect("expire"), 2);
    assert!(storage.load("old").await.expect("load").is_empty());
    assert!(storage.load("never").await.expect("load").is_empty());
    assert_eq!(storage.load("new").await.expect("load").len(), 1);
}

/// Checks that records and the sequence survive reopening a persistent
/// backend. Passes trivially for backends that aren't persistent.
///
/// Args:
///     fixture: Opens the backend.
pub async fn records_survive_reopen<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    let caps = storage.capabilities();
    if !caps.persistent {
        return;
    }
    for n in 1..=3 {
        storage
            .append("t", record(&format!("m{n}"), n), caps.max_durability)
            .await
            .expect("append");
    }
    drop(storage);
    let storage = fixture.open().await;
    assert_eq!(all_seqs(&storage, "t").await, vec![1, 2, 3]);
    let again = storage
        .append("t", record("m2", 9), caps.max_durability)
        .await
        .expect("append")
        .seq;
    assert_eq!(again, 2);
    let next = storage
        .append("t", record("m4", 9), caps.max_durability)
        .await
        .expect("append")
        .seq;
    assert_eq!(next, 4);
}

/// Checks that cursors and session activity survive reopening a persistent
/// backend. Passes trivially for backends that aren't persistent.
///
/// Args:
///     fixture: Opens the backend.
pub async fn cursors_survive_reopen<F: Fixture>(fixture: F) {
    let storage = fixture.open().await;
    if !storage.capabilities().persistent {
        return;
    }
    storage.subscribe("s", "t", 0).await.expect("subscribe");
    storage.ack("s", "t", 4).await.expect("ack");
    storage.touch("s", 1_000).await.expect("touch");
    drop(storage);
    let storage = fixture.open().await;
    assert_eq!(storage.load("s").await.expect("load")[0].acked_seq, 4);
    assert_eq!(storage.expire(500).await.expect("expire"), 0);
}

/// Generates one `#[test]` per conformance check for a backend.
///
/// Call it inside a test module with an expression that builds a fresh
/// `Fixture`. The expression is evaluated once per test.
#[macro_export]
macro_rules! conformance_tests {
    ($fixture:expr) => {
        $crate::conformance_tests!(@checks $fixture;
            seq_starts_at_one_and_increments,
            seq_is_never_reused_after_trim,
            read_respects_from_and_max,
            read_past_end_or_unknown_topic_is_empty,
            read_from_trimmed_seq_starts_at_oldest,
            trim_by_count_keeps_newest,
            trim_by_age_keeps_recent,
            trim_by_bytes_keeps_what_fits,
            duplicate_message_id_returns_original_seq,
            publish_times_never_decrease_and_seq_at_finds_them,
            topics_lists_every_log,
            topics_are_independent,
            concurrent_appends_are_gap_free,
            unsupported_durability_is_refused,
            cursors_only_move_forward,
            ack_without_subscription_is_ignored,
            subscribe_replaces_and_unsubscribe_removes,
            sessions_are_separate_and_sorted,
            expire_removes_idle_sessions,
            records_survive_reopen,
            cursors_survive_reopen,
        );
    };
    (@checks $fixture:expr; $($check:ident),* $(,)?) => {
        $(
            /// Runs the conformance check of the same name against this backend.
            #[test]
            fn $check() {
                $crate::conformance::block_on($crate::conformance::$check($fixture));
            }
        )*
    };
}
