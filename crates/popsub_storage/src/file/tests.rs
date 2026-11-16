//! Tests for `FileStorage`: the conformance suite, recovery from interrupted
//! writes and damage, trimming across reopens, the directory lock, topic
//! names, the session log, and a crash test that kills a writer process.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::conformance::{Fixture, block_on};
use crate::error::StorageError;
use crate::file::codec::{self, Decoded};
use crate::file::sessions::SessionLog;
use crate::file::{FileOptions, FileStorage, MAX_TOPIC_BYTES, topic_dir_name, topic_from_dir_name};
use crate::record::NewRecord;
use crate::storage::{Durability, MessageLog, Retention, SubscriptionStore};

/// Segment size used by most tests, small enough that a few records roll
/// over to new segment files.
const SMALL_SEGMENTS: FileOptions = FileOptions { segment_bytes: 256 };

/// The environment variable that turns `crash_child` into the writer
/// process of `synced_appends_survive_the_writer_being_killed`.
const CRASH_CHILD_DIR: &str = "POPSUB_CRASH_CHILD_DIR";

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

/// The writer process for the crash test. Does nothing unless started by
/// that test with `CRASH_CHILD_DIR` set; then appends synced records to
/// topic `t` forever, printing `confirmed <seq>` after each one returns.
#[test]
fn crash_child() {
    let Ok(dir) = std::env::var(CRASH_CHILD_DIR) else {
        return;
    };
    block_on(async move {
        let storage = open(Path::new(&dir)).await;
        let mut stdout = std::io::stdout();
        let mut n = 0u64;
        loop {
            n += 1;
            let stored = storage
                .append(
                    "t",
                    record(&format!("c{n}-{}", std::process::id())),
                    Durability::Synced,
                )
                .await
                .expect("append");
            writeln!(stdout, "confirmed {} {}", stored.seq, stored.payload).expect("stdout");
            stdout.flush().expect("flush");
        }
    });
}

/// Checks that every append confirmed at `Durability::Synced` is readable
/// after the writing process is killed at an arbitrary point, over several
/// kill and restart rounds in the same directory.
#[test]
fn synced_appends_survive_the_writer_being_killed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let exe = std::env::current_exe().expect("test binary");
    let mut confirmed: Vec<(u64, String)> = Vec::new();
    for round in 0..4u64 {
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos() as usize;
        let stop_after = 15 + (jitter + round as usize * 37) % 60;
        let mut child = Command::new(&exe)
            .args([
                "--exact",
                "file::tests::crash_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_CHILD_DIR, dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the writer");
        let stdout = child.stdout.take().expect("stdout");
        let mut seen = 0;
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("line");
            let mut parts = line.split(' ');
            if parts.next() != Some("confirmed") {
                continue;
            }
            let seq = parts.next().and_then(|s| s.parse().ok()).expect("seq");
            let payload = parts.next().expect("payload").to_string();
            confirmed.push((seq, payload));
            seen += 1;
            if seen == stop_after {
                break;
            }
        }
        child.kill().expect("kill the writer");
        child.wait().expect("reap the writer");
        assert!(seen > 0, "the writer confirmed nothing in round {round}");
    }
    block_on(async move {
        let storage = open(dir.path()).await;
        let stored = storage.read("t", 0, usize::MAX).await.expect("read");
        for (seq, payload) in &confirmed {
            let record = stored
                .iter()
                .find(|r| r.seq == *seq)
                .unwrap_or_else(|| panic!("confirmed seq {seq} is missing"));
            assert_eq!(&record.payload, payload, "seq {seq}");
        }
        let all: Vec<u64> = stored.iter().map(|r| r.seq).collect();
        assert_eq!(all, (1..=all.len() as u64).collect::<Vec<_>>());
    });
}
