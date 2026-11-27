//! Checks that this build opens data directories written by every earlier
//! release, and can keep writing to them.
//!
//! Each directory under `tests/fixtures/upgrade/` was written by the release
//! it is named for, with `scripts/upgrade-fixture` (`make upgrade-fixture`),
//! and holds what a server's `storage.path` holds: topic logs, the session
//! log and `users.json`. A new release adds its own directory before the
//! next one can change the format, so this test fails as soon as a change
//! stops reading what an earlier release wrote. The contents it checks are
//! the ones that program writes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use popsub_auth::{AuthConfig, AuthService, FileUserStore, UserStore};
use popsub_storage::{
    Durability, FileStorage, MessageLog, NewRecord, PayloadEncoding, Storage, SubscriptionStore,
};

/// The publish time of the fixture's first record, as `upgrade-fixture` sets it.
const START_MS: i64 = 1_700_000_000_000;

/// Lists the fixture directories, oldest release first.
///
/// Returns:
///     One path per release.
fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the fixtures directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Copies a directory tree, so a test can open a fixture without changing it.
///
/// Args:
///     from: The directory to copy.
///     to: Where to put the copy. It is created.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create a directory");
    for entry in std::fs::read_dir(from).expect("list a directory") {
        let path = entry.expect("an entry").path();
        let target = to.join(path.file_name().expect("a file name"));
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy a file");
        }
    }
}

/// Reads a topic's retained records as `(seq, payload, encoding)`.
///
/// Args:
///     storage: The store.
///     topic: The topic.
///
/// Returns:
///     The records in `seq` order.
async fn contents(
    storage: &FileStorage,
    topic: &str,
) -> Vec<(u64, String, Option<PayloadEncoding>)> {
    storage
        .read(topic, 0, usize::MAX)
        .await
        .expect("read")
        .into_iter()
        .map(|r| (r.seq, r.payload, r.encoding))
        .collect()
}

/// Checks the topic logs and sessions of one opened fixture, then appends to
/// it, reopens it and backs it up.
///
/// Args:
///     dir: The copied fixture.
///     release: The fixture's name, for failure messages.
async fn check_storage(dir: &Path, release: &str) {
    let storage = FileStorage::open(dir)
        .await
        .unwrap_or_else(|e| panic!("{release}: opening failed: {e}"));
    let orders = storage.read("orders", 0, usize::MAX).await.expect("read");
    let expected: Vec<(u64, String, Option<PayloadEncoding>)> = (4..=12)
        .map(|n| {
            if n == 5 {
                (n, "aGk=".to_string(), Some(PayloadEncoding::Base64))
            } else {
                (n, format!("message {n}"), None)
            }
        })
        .collect();
    assert_eq!(contents(&storage, "orders").await, expected, "{release}");
    for record in &orders {
        assert_eq!(record.message_id, format!("m{}", record.seq), "{release}");
        assert_eq!(record.publisher, "alice", "{release}");
        assert_eq!(record.qos, 1, "{release}");
        assert_eq!(
            record.published_at_ms,
            START_MS + record.seq as i64 * 1000,
            "{release}"
        );
    }
    let sensors: Vec<u64> = contents(&storage, "sensors/1/temp")
        .await
        .iter()
        .map(|(seq, _, _)| *seq)
        .collect();
    assert_eq!(sensors, vec![1, 2, 3], "{release}");
    assert!(contents(&storage, "purged").await.is_empty(), "{release}");
    assert_eq!(
        storage.bounds("purged").await.expect("bounds").next_seq(),
        3,
        "{release}"
    );
    let session = storage.load("alice|laptop").await.expect("load");
    let cursors: Vec<(String, u64)> = session
        .into_iter()
        .map(|s| (s.topic, s.acked_seq))
        .collect();
    assert_eq!(
        cursors,
        vec![("orders".to_string(), 7), ("sensors/1/temp".to_string(), 0)],
        "{release}"
    );

    let appended = storage
        .append(
            "orders",
            NewRecord {
                message_id: "after-upgrade".to_string(),
                publisher: "alice".to_string(),
                payload: "new".to_string(),
                encoding: None,
                qos: 1,
                published_at_ms: START_MS + 60_000,
            },
            Durability::Synced,
        )
        .await
        .expect("append after upgrading");
    assert_eq!(appended.seq, 13, "{release}");
    let retried = storage
        .append(
            "orders",
            NewRecord {
                message_id: "m12".to_string(),
                publisher: "alice".to_string(),
                payload: "message 12".to_string(),
                encoding: None,
                qos: 1,
                published_at_ms: START_MS + 61_000,
            },
            Durability::Synced,
        )
        .await
        .expect("retry an old message id");
    assert_eq!(
        retried.seq, 12,
        "{release}: a retry of an old id is a duplicate"
    );
    drop(storage);

    let storage = FileStorage::open(dir).await.expect("reopen");
    assert_eq!(
        storage.bounds("orders").await.expect("bounds").last_seq,
        13,
        "{release}"
    );
    let backup = dir.with_file_name("backup");
    let summary = storage.backup(&backup).await.expect("back up");
    assert_eq!(summary.records, 10 + 3, "{release}");
}

/// Checks the accounts of one copied fixture, through the same store and
/// service the server uses.
///
/// Args:
///     dir: The copied fixture.
///     release: The fixture's name, for failure messages.
async fn check_users(dir: &Path, release: &str) {
    let store = FileUserStore::open(dir.join("users.json"))
        .unwrap_or_else(|e| panic!("{release}: users.json didn't open: {e}"));
    let mut names = store.list_usernames().await.expect("list");
    names.sort();
    assert_eq!(names, vec!["admin", "gone", "rita", "viewer"], "{release}");
    let auth = AuthService::with_store(
        AuthConfig::new("upgrade-test-secret").with_admin("admin", "admin-password"),
        Arc::new(store),
    )
    .await;
    for name in ["admin", "rita", "viewer"] {
        auth.login(name, &format!("{name}-password"))
            .await
            .unwrap_or_else(|e| panic!("{release}: {name} can't log in: {e}"));
    }
    assert!(
        auth.login("gone", "gone-password").await.is_err(),
        "{release}"
    );
    assert!(auth.can_subscribe("rita", "news/today").await, "{release}");
    assert!(!auth.can_publish("rita", "news/today").await, "{release}");
    assert!(auth.can_publish("rita", "rita/notes").await, "{release}");
    assert!(
        auth.can_subscribe("viewer", "metrics/cpu").await,
        "{release}"
    );
    assert!(
        !auth.can_publish("viewer", "metrics/cpu").await,
        "{release}"
    );
    auth.update_user_password("rita", "rita-new-password")
        .await
        .expect("change a password after upgrading");
    let reopened = FileUserStore::open(dir.join("users.json")).expect("reopen users.json");
    assert!(
        reopened
            .get_user("rita")
            .await
            .expect("get")
            .expect("rita")
            .token_version
            > 0,
        "{release}"
    );
}

/// Checks that every earlier release's data directory opens, reads back
/// exactly, takes new records and account changes, and can be backed up.
#[tokio::test(flavor = "multi_thread")]
async fn data_written_by_earlier_releases_still_opens() {
    let fixtures = fixtures();
    assert!(!fixtures.is_empty(), "no upgrade fixtures found");
    for fixture in fixtures {
        let release = fixture
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        let temp = tempfile::tempdir().expect("temp dir");
        let dir = temp.path().join("data");
        copy_tree(&fixture, &dir);
        check_storage(&dir, &release).await;
        check_users(&dir, &release).await;
    }
}
