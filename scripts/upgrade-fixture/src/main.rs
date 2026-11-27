//! Writes the data directory that `crates/popsub/tests/upgrade.rs` opens, with
//! the storage and account code of the release being cut.
//!
//! Run it from a checkout of a release tag, before the next release changes
//! the on-disk format, with `make upgrade-fixture`. It only uses APIs every
//! release since 0.1.0 has had, so the same program builds against any of
//! them. The contents are fixed, apart from password hashes and account
//! creation times, so the test can check them exactly.

use std::path::PathBuf;

use popsub_auth::store::hash_password;
use popsub_auth::{FileUserStore, Role, TopicPermission, User, UserStore};
use popsub_storage::{
    Durability, FileOptions, FileStorage, MessageLog, NewRecord, PayloadEncoding, Retention,
    SubscriptionStore,
};

/// The publish time of the first record in the fixture.
const START_MS: i64 = 1_700_000_000_000;

/// Builds one of the fixture's records.
///
/// Args:
///     n: The record's number, used in its id, payload and publish time.
///     qos: Its delivery level.
///
/// Returns:
///     A record from `alice`.
fn record(n: i64, qos: u8) -> NewRecord {
    NewRecord {
        message_id: format!("m{n}"),
        publisher: "alice".to_string(),
        payload: format!("message {n}"),
        encoding: None,
        qos,
        published_at_ms: START_MS + n * 1000,
    }
}

/// Builds an account with a fresh hash of `password`.
///
/// Args:
///     username: The username.
///     password: The password.
///     role: The role.
///     enabled: Whether it may log in.
///
/// Returns:
///     The account.
fn user(username: &str, password: &str, role: Role, enabled: bool) -> User {
    let mut user = User::new(
        username,
        hash_password(password).expect("hash a password"),
        role,
    );
    user.enabled = enabled;
    user
}

/// Writes the fixture into the directory named by the first argument, which
/// must not exist yet.
///
/// Topic `orders` gets 12 records in 256-byte segments, one of them base64,
/// trimmed to the last 9. `sensors/1/temp` gets 3 QoS 0 records, and
/// `purged` gets 2 that are then all trimmed. Session `alice|laptop`
/// subscribes to two topics and acknowledges up to 7 on `orders`.
/// `users.json` holds an admin, a user with grants, a read-only user and a
/// disabled user, each with the password `<username>-password`.
#[tokio::main]
async fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: upgrade-fixture <new directory>"),
    );
    assert!(!out.exists(), "{} already exists", out.display());
    let storage = FileStorage::open_with(&out, FileOptions { segment_bytes: 256 })
        .await
        .expect("open the store");
    for n in 1..=12 {
        let mut record = record(n, 1);
        if n == 5 {
            record.payload = "aGk=".to_string();
            record.encoding = Some(PayloadEncoding::Base64);
        }
        storage
            .append("orders", record, Durability::Synced)
            .await
            .expect("append to orders");
    }
    let keep_nine = Retention {
        max_messages: Some(9),
        ..Retention::default()
    };
    storage
        .trim("orders", &keep_nine, 0)
        .await
        .expect("trim orders");
    for n in 1..=3 {
        storage
            .append("sensors/1/temp", record(n, 0), Durability::Synced)
            .await
            .expect("append to sensors");
    }
    for n in 1..=2 {
        storage
            .append("purged", record(n, 1), Durability::Synced)
            .await
            .expect("append to purged");
    }
    let nothing = Retention {
        max_messages: Some(0),
        ..Retention::default()
    };
    storage
        .trim("purged", &nothing, 0)
        .await
        .expect("trim purged");
    storage
        .subscribe("alice|laptop", "orders", 3)
        .await
        .expect("subscribe");
    storage.ack("alice|laptop", "orders", 7).await.expect("ack");
    storage
        .subscribe("alice|laptop", "sensors/1/temp", 0)
        .await
        .expect("subscribe");
    storage
        .touch("alice|laptop", START_MS + 100_000)
        .await
        .expect("touch");
    drop(storage);

    let users = FileUserStore::open(out.join("users.json")).expect("open users.json");
    for account in [
        user("admin", "admin-password", Role::admin(), true),
        user(
            "rita",
            "rita-password",
            Role::user(vec![
                TopicPermission::read_only("news/#"),
                TopicPermission::full_access("rita/#"),
            ]),
            true,
        ),
        user(
            "viewer",
            "viewer-password",
            Role::read_only(vec!["metrics/#".to_string()]),
            true,
        ),
        user("gone", "gone-password", Role::admin(), false),
    ] {
        users.add_user(account).await.expect("add an account");
    }
    std::fs::remove_file(out.join("LOCK")).expect("remove the lock file");
    println!("wrote {}", out.display());
}
