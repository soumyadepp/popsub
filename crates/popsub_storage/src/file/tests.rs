//! Tests for `FileStorage`: the conformance suite, recovery from interrupted
//! writes and damage, trimming across reopens, the directory lock, topic
//! names and the session log. The crash test that kills a writer process
//! is in `tests/crash.rs`, a separate test binary; see there for why.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::conformance::{Fixture, block_on};
use crate::error::StorageError;
use crate::file::body;
use crate::file::codec::{self, Decoded};
use crate::file::sessions::SessionLog;
use crate::file::{FileOptions, FileStorage, MAX_TOPIC_BYTES, topic_dir_name, topic_from_dir_name};
use crate::record::{NewRecord, PayloadEncoding, Record};
use crate::storage::{Durability, MessageLog, Retention, SubscriptionStore};

/// Segment size used by most tests, small enough that a few records roll
/// over to new segment files.
const SMALL_SEGMENTS: FileOptions = FileOptions { segment_bytes: 256 };

/// Opens a `FileStorage` in a temporary directory it owns. Opening it again
/// reaches the same directory, which is how the suite checks persistence.
struct TempDirFixture(tempfile::TempDir);

impl TempDirFixture {
    /// Creates a fixture with a new temporary directory.
    ///
    /// Returns:
    ///     The fixture.
    fn new() -> Self {
        Self(tempfile::tempdir().expect("temp dir"))
    }
}

impl Fixture for TempDirFixture {
    /// The backend under test.
    type Storage = FileStorage;

    /// Opens the store in the fixture's directory with small segments.
    ///
    /// Returns:
    ///     The open store.
    async fn open(&self) -> FileStorage {
        FileStorage::open_with(self.0.path(), SMALL_SEGMENTS)
            .await
            .expect("open file storage")
    }
}

/// The conformance suite against `FileStorage`.
mod file_conformance {
    use super::TempDirFixture;

    crate::conformance_tests!(TempDirFixture::new());
}

/// Builds a record to append.
///
/// Args:
///     id: The message id, also used as the payload.
///
/// Returns:
///     A QoS 1 record from `alice`.
fn record(id: &str) -> NewRecord {
    NewRecord {
        message_id: id.to_string(),
        publisher: "alice".to_string(),
        payload: id.to_string(),
        encoding: None,
        qos: 1,
        published_at_ms: 1,
    }
}

/// Opens a store with small segments in a directory.
///
/// Args:
///     dir: The directory.
///
/// Returns:
///     The open store.
async fn open(dir: &Path) -> FileStorage {
    FileStorage::open_with(dir, SMALL_SEGMENTS)
        .await
        .expect("open file storage")
}

/// Appends records `m1` to `mN` to topic `t` at `Durability::Synced`.
///
/// Args:
///     storage: The store.
///     from: The first number.
///     to: The last number.
async fn append_range(storage: &FileStorage, from: u64, to: u64) {
    for n in from..=to {
        storage
            .append("t", record(&format!("m{n}")), Durability::Synced)
            .await
            .expect("append");
    }
}

/// Reads every retained `seq` of topic `t`.
///
/// Args:
///     storage: The store.
///
/// Returns:
///     The sequence numbers.
async fn seqs(storage: &FileStorage) -> Vec<u64> {
    storage
        .read("t", 0, usize::MAX)
        .await
        .expect("read")
        .iter()
        .map(|r| r.seq)
        .collect()
}

/// Finds the files of topic `t`'s directory with a given extension, sorted.
///
/// Args:
///     root: The store's directory.
///     extension: The extension, such as `seg`.
///
/// Returns:
///     The paths.
fn topic_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    let dir = root.join("topics").join(topic_dir_name("t"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("topic dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == extension))
        .collect();
    files.sort();
    files
}

/// Checks that frames round-trip and that a short, oversized or damaged frame is torn.
#[test]
fn frames_round_trip_and_damage_is_detected() {
    let frame = codec::encode(b"hello");
    assert_eq!(
        codec::decode(&frame, 0),
        Decoded::Frame {
            body: b"hello",
            next: frame.len()
        }
    );
    assert_eq!(codec::decode(&frame, frame.len()), Decoded::End);
    assert_eq!(codec::decode(&frame[..frame.len() - 1], 0), Decoded::Torn);
    assert_eq!(codec::decode(&frame[..3], 0), Decoded::Torn);
    let mut flipped = frame.clone();
    *flipped.last_mut().unwrap() ^= 1;
    assert_eq!(codec::decode(&flipped, 0), Decoded::Torn);
    let mut oversized = frame.clone();
    oversized[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(codec::decode(&oversized, 0), Decoded::Torn);
}

/// Checks that any topic name maps to a safe directory name and back.
#[test]
fn topic_names_round_trip_through_directory_names() {
    for topic in ["chat", "", "a/b/../c", "metrics/#", "日本語", "with space"] {
        let name = topic_dir_name(topic);
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric()), "{name}");
        assert_eq!(topic_from_dir_name(&name).as_deref(), Some(topic));
    }
    for name in ["chat", "tz", "t0", "LOCK", "tc3"] {
        assert_eq!(topic_from_dir_name(name), None, "{name}");
    }
}

/// Checks that records roll over into several segments and all read back after reopening.
#[test]
fn records_span_segments_and_survive_reopening() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = open(dir.path()).await;
        append_range(&storage, 1, 20).await;
        drop(storage);
        assert!(topic_files(dir.path(), "seg").len() > 2);
        let storage = open(dir.path()).await;
        assert_eq!(seqs(&storage).await, (1..=20).collect::<Vec<_>>());
        let again = storage
            .append("t", record("m7"), Durability::Synced)
            .await
            .expect("append");
        assert_eq!(again.seq, 7);
    });
}

/// Checks that a half-written frame at the end of the newest segment is cut
/// off on open, and appends carry on from the last whole record.
#[test]
fn an_interrupted_write_is_truncated_on_open() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = open(dir.path()).await;
        append_range(&storage, 1, 3).await;
        drop(storage);
        let newest = topic_files(dir.path(), "seg").pop().expect("a segment");
        let partial = &codec::encode(b"{\"never\":\"finished\"}")[..10];
        OpenOptions::new()
            .append(true)
            .open(&newest)
            .expect("open segment")
            .write_all(partial)
            .expect("write partial frame");

        let storage = open(dir.path()).await;
        assert_eq!(seqs(&storage).await, vec![1, 2, 3]);
        append_range(&storage, 4, 4).await;
        drop(storage);
        let storage = open(dir.path()).await;
        assert_eq!(seqs(&storage).await, vec![1, 2, 3, 4]);
    });
}

/// Checks that damage in an older segment stops the store from opening.
#[test]
fn damage_in_an_older_segment_refuses_to_open() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = open(dir.path()).await;
        append_range(&storage, 1, 20).await;
        drop(storage);
        let oldest = topic_files(dir.path(), "seg").remove(0);
        let mut bytes = std::fs::read(&oldest).expect("read segment");
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        std::fs::write(&oldest, bytes).expect("write segment");
        let result = FileStorage::open_with(dir.path(), SMALL_SEGMENTS).await;
        assert!(
            matches!(result, Err(StorageError::Corrupt(_))),
            "{result:?}"
        );
    });
}

/// Checks that trimming deletes segments left with no records, that trimmed
/// records stay gone after reopening, and that numbering carries on even
/// when everything was trimmed.
#[test]
fn trimming_deletes_segments_and_survives_reopening() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = open(dir.path()).await;
        append_range(&storage, 1, 20).await;
        let before = topic_files(dir.path(), "seg").len();
        let keep_three = Retention {
            max_messages: Some(3),
            ..Retention::default()
        };
        assert_eq!(storage.trim("t", &keep_three, 0).await.expect("trim"), 17);
        assert!(topic_files(dir.path(), "seg").len() < before);
        drop(storage);

        let storage = open(dir.path()).await;
        assert_eq!(seqs(&storage).await, vec![18, 19, 20]);
        let none = Retention {
            max_messages: Some(0),
            ..Retention::default()
        };
        storage.trim("t", &none, 0).await.expect("trim");
        drop(storage);

        let storage = open(dir.path()).await;
        assert!(seqs(&storage).await.is_empty());
        let bounds = storage.bounds("t").await.expect("bounds");
        assert_eq!(bounds.next_seq(), 21);
        let next = storage
            .append("t", record("later"), Durability::Synced)
            .await
            .expect("append");
        assert_eq!(next.seq, 21);
    });
}

/// Checks that a second store can't open a directory that is already open,
/// and can once the first is dropped.
#[test]
fn a_directory_can_only_be_open_once() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = open(dir.path()).await;
        let second = FileStorage::open_with(dir.path(), SMALL_SEGMENTS).await;
        assert!(matches!(second, Err(StorageError::Unavailable(_))));
        drop(first);
        assert!(FileStorage::open(dir.path()).await.is_ok());
    });
}

/// Checks that unusual topic names are stored and listed after reopening,
/// and that a name too long for a directory is refused.
#[test]
fn unusual_topic_names_survive_reopening() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = open(dir.path()).await;
        let names = ["a/b/../c", "metrics/#", "日本語"];
        for name in names {
            storage
                .append(name, record("x"), Durability::Buffered)
                .await
                .expect("append");
        }
        let long = "x".repeat(MAX_TOPIC_BYTES + 1);
        let refused = storage.append(&long, record("x"), Durability::None).await;
        assert!(matches!(refused, Err(StorageError::Unsupported(_))));
        drop(storage);
        let storage = open(dir.path()).await;
        let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        expected.sort();
        assert_eq!(storage.topics().await.expect("topics"), expected);
    });
}

/// Checks that the session log compacts once it is large and that the
/// state is the same after compaction and reopening.
#[test]
fn the_session_log_compacts_and_keeps_its_state() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("sessions.log");
    let mut log = SessionLog::open(&path).expect("open");
    log.subscribe("s", "t", 0).expect("subscribe");
    log.touch("s", 5).expect("touch");
    for seq in 1..=1500 {
        log.ack("s", "t", seq).expect("ack");
    }
    assert!(log.ops() < 1000, "{} ops after compaction", log.ops());
    log.ack("s", "t", 1500).expect("ack");
    drop(log);
    let log = SessionLog::open(&path).expect("reopen");
    let subs = log.load("s");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].acked_seq, 1500);
}

/// Checks that a half-written change at the end of the session log is dropped on open.
#[test]
fn a_torn_session_log_keeps_everything_before_the_tear() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("sessions.log");
    let mut log = SessionLog::open(&path).expect("open");
    log.subscribe("s", "t", 3).expect("subscribe");
    drop(log);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open log")
        .write_all(&[1, 2, 3])
        .expect("write");
    let mut log = SessionLog::open(&path).expect("reopen");
    assert_eq!(log.load("s")[0].acked_seq, 3);
    log.ack("s", "t", 4).expect("ack");
    drop(log);
    assert_eq!(
        SessionLog::open(&path).expect("reopen").load("s")[0].acked_seq,
        4
    );
}

/// Checks that sessions written through `FileStorage` survive reopening.
#[test]
fn sessions_survive_reopening() {
    block_on(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = open(dir.path()).await;
        storage.subscribe("s", "t", 2).await.expect("subscribe");
        storage.ack("s", "t", 9).await.expect("ack");
        storage.touch("old", 1).await.expect("touch");
        assert_eq!(storage.expire(10).await.expect("expire"), 2);
        storage.subscribe("kept", "t", 4).await.expect("subscribe");
        storage.touch("kept", 50).await.expect("touch");
        drop(storage);
        let storage = open(dir.path()).await;
        assert!(storage.load("s").await.expect("load").is_empty());
        assert_eq!(storage.load("kept").await.expect("load")[0].acked_seq, 4);
    });
}

/// Builds a stored record for the format tests.
///
/// Args:
///     message_id: The id.
///     publisher: The publisher.
///     payload: The payload.
///
/// Returns:
///     A record on topic `t` with `seq` 7.
fn stored(message_id: &str, publisher: &str, payload: &str) -> Record {
    Record {
        topic: "t".to_string(),
        seq: 7,
        message_id: message_id.to_string(),
        publisher: publisher.to_string(),
        payload: payload.to_string(),
        encoding: None,
        qos: 1,
        published_at_ms: 1_795_700_000_123,
    }
}

/// Checks that every kind of record reads back exactly from the binary
/// form, including ids that look like UUIDs but aren't written the way the
/// server writes one, and fields too long for the binary form.
#[test]
fn records_read_back_exactly_from_the_binary_form() {
    let mut base64 = stored("custom-id", "alice", "aGVsbG8=");
    base64.encoding = Some(PayloadEncoding::Base64);
    base64.qos = 0;
    let long_publisher = "p".repeat(70_000);
    let records = [
        stored("8c9d5cf2-2fa3-4fc1-bc27-d9b5de9da192", "alice", "hello"),
        stored(
            "8C9D5CF2-2FA3-4FC1-BC27-D9B5DE9DA192",
            "alice",
            "upper case id",
        ),
        stored(
            "8c9d5cf22fa34fc1bc27d9b5de9da192",
            "alice",
            "id without hyphens",
        ),
        stored("custom-id", "bob", ""),
        stored("", "", "quotes \" and \n newlines, and caf\u{e9}"),
        stored("custom-id", &long_publisher, "too long for the binary form"),
        base64,
    ];
    for record in records {
        let mut bytes = Vec::new();
        body::encode(&record, &mut bytes);
        assert_eq!(body::decode(&bytes, "t").expect("decodes"), record);
    }
    let mut bytes = Vec::new();
    body::encode(
        &stored("8c9d5cf2-2fa3-4fc1-bc27-d9b5de9da192", "alice", ""),
        &mut bytes,
    );
    assert!(
        bytes.len() < 60,
        "a server-assigned id is stored as 16 bytes"
    );
    assert!(body::decode(&bytes[..bytes.len() - 3], "t").is_err());
    assert!(body::decode(b"", "t").is_err());
}

/// Checks that a log holding JSON records, as older versions wrote, opens,
/// reads back, and takes new records after them.
#[test]
fn a_log_written_as_json_still_reads() {
    let dir = tempfile::tempdir().expect("temp dir");
    block_on(async {
        let storage = open(dir.path()).await;
        append_range(&storage, 1, 1).await;
    });
    let segment = topic_files(dir.path(), "seg").pop().expect("a segment");
    let mut old = stored("m2", "alice", "m2");
    old.seq = 2;
    old.published_at_ms = 1;
    OpenOptions::new()
        .append(true)
        .open(&segment)
        .expect("segment")
        .write_all(&codec::encode(old.to_json().as_bytes()))
        .expect("write");
    block_on(async {
        let storage = open(dir.path()).await;
        append_range(&storage, 3, 3).await;
        let records = storage.read("t", 0, usize::MAX).await.expect("read");
        let ids: Vec<&str> = records.iter().map(|r| r.message_id.as_str()).collect();
        assert_eq!(ids, ["m1", "m2", "m3"]);
        assert_eq!(records[1], old);
    });
}
