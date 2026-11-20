//! The crash test for `FileStorage`: a child process appends with
//! `Durability::Synced` and is killed at arbitrary points, and every append
//! it confirmed must still be readable.
//!
//! This lives in its own test binary on purpose. Spawning a child process
//! briefly gives the child copies of the parent's open file descriptors, the
//! store's lock file included, until the child starts its own program. Run
//! alongside the other storage tests, a test that closes a store and opens
//! it again could land in that window and find the directory still locked.
//! Cargo runs test binaries one after another, so nothing else runs here
//! while children are being spawned.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use popsub_storage::{Durability, FileOptions, FileStorage, MessageLog, NewRecord};

/// The environment variable that turns `crash_child` into the writer
/// process of `synced_appends_survive_the_writer_being_killed`.
const CRASH_CHILD_DIR: &str = "POPSUB_CRASH_CHILD_DIR";

/// Segment size small enough that the writer rolls over to new segment
/// files while it runs.
const SMALL_SEGMENTS: FileOptions = FileOptions { segment_bytes: 256 };

/// Runs a future to completion on a new multi-threaded runtime.
///
/// Args:
///     work: The future.
///
/// Returns:
///     Its output.
fn block_on<T>(work: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .expect("runtime")
        .block_on(work)
}

/// Opens a store with small segments.
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

/// The writer process for the crash test. Does nothing unless started by
/// that test with `CRASH_CHILD_DIR` set; then appends synced records to
/// topic `t` forever, printing `confirmed <seq> <payload>` after each one
/// returns.
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
            let id = format!("c{n}-{}", std::process::id());
            let record = NewRecord {
                message_id: id.clone(),
                publisher: "alice".to_string(),
                payload: id,
                encoding: None,
                qos: 1,
                published_at_ms: 1,
            };
            let stored = storage
                .append("t", record, Durability::Synced)
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
            .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
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
