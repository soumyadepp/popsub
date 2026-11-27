//! Tests for the storage types, and the conformance suite run against
//! `MemoryStorage`.

use crate::conformance::Fixture;
use crate::{
    Capabilities, Durability, LogBounds, MemoryStorage, MessageLog, NewRecord, PayloadEncoding,
    Record, Retention, Storage, StorageError,
};

/// Opens a new, empty `MemoryStorage` every time, since it can't be reopened.
struct MemoryFixture;

impl Fixture for MemoryFixture {
    /// The backend under test.
    type Storage = MemoryStorage;

    /// Creates an empty in-memory store.
    ///
    /// Returns:
    ///     A new `MemoryStorage`.
    async fn open(&self) -> MemoryStorage {
        MemoryStorage::new()
    }
}

/// The conformance suite against `MemoryStorage`.
mod memory_conformance {
    use super::MemoryFixture;

    crate::conformance_tests!(MemoryFixture);
}

/// Builds a record with the given sizes of variable-length fields.
///
/// Args:
///     id: The message id.
///     publisher: The publisher.
///     payload: The payload.
///
/// Returns:
///     A stored record with `seq` 1.
fn stored(id: &str, publisher: &str, payload: &str) -> Record {
    let new = NewRecord {
        message_id: id.to_string(),
        publisher: publisher.to_string(),
        payload: payload.to_string(),
        encoding: None,
        qos: 0,
        published_at_ms: 0,
    };
    Record::from_new("t", 1, new, 0)
}

/// Record size counts the message id, publisher and payload bytes.
#[test]
fn record_size_counts_variable_fields() {
    assert_eq!(stored("ab", "cde", "fghij").size(), 10);
}

/// Bounds of an unknown topic are empty and the next seq is 1.
#[test]
fn empty_bounds_start_at_one() {
    assert!(LogBounds::EMPTY.is_empty());
    assert_eq!(LogBounds::EMPTY.len(), 0);
    assert_eq!(LogBounds::EMPTY.next_seq(), 1);
}

/// Bounds count the retained records and continue after the last one.
#[test]
fn bounds_count_retained_records() {
    let bounds = LogBounds {
        first_seq: 4,
        last_seq: 6,
    };
    assert!(!bounds.is_empty());
    assert_eq!(bounds.len(), 3);
    assert_eq!(bounds.next_seq(), 7);
    let trimmed = LogBounds {
        first_seq: 7,
        last_seq: 6,
    };
    assert!(trimmed.is_empty());
    assert_eq!(trimmed.len(), 0);
}

/// Durability levels are ordered from weakest to strongest.
#[test]
fn durability_is_ordered() {
    assert!(Durability::None < Durability::Buffered);
    assert!(Durability::Buffered < Durability::Synced);
}

/// A backend accepts its own durability level and weaker ones, and refuses stronger.
#[test]
fn capabilities_check_refuses_stronger_durability() {
    let caps = Capabilities {
        max_durability: Durability::Buffered,
        persistent: true,
    };
    assert!(caps.check(Durability::None).is_ok());
    assert!(caps.check(Durability::Buffered).is_ok());
    assert!(matches!(
        caps.check(Durability::Synced),
        Err(StorageError::Unsupported(_))
    ));
}

/// A retention policy with no limits never trims.
#[test]
fn retention_without_limits_keeps_everything() {
    assert!(!Retention::default().must_trim(1_000_000, u64::MAX, i64::MIN, i64::MAX));
}

/// An empty log is never trimmed, whatever the limits.
#[test]
fn retention_never_trims_an_empty_log() {
    let policy = Retention {
        max_messages: Some(0),
        max_age_ms: Some(0),
        max_bytes: Some(0),
    };
    assert!(!policy.must_trim(0, 0, 0, 100));
}

/// Each limit trims on its own when exceeded, and not at the limit.
#[test]
fn retention_limits_apply_independently() {
    let by_count = Retention {
        max_messages: Some(2),
        ..Retention::default()
    };
    assert!(!by_count.must_trim(2, 0, 0, 0));
    assert!(by_count.must_trim(3, 0, 0, 0));
    let by_bytes = Retention {
        max_bytes: Some(10),
        ..Retention::default()
    };
    assert!(!by_bytes.must_trim(1, 10, 0, 0));
    assert!(by_bytes.must_trim(1, 11, 0, 0));
    let by_age = Retention {
        max_age_ms: Some(100),
        ..Retention::default()
    };
    assert!(!by_age.must_trim(1, 0, 900, 1_000));
    assert!(by_age.must_trim(1, 0, 899, 1_000));
}

/// An age limit too large for `i64` doesn't overflow.
#[test]
fn retention_age_limit_does_not_overflow() {
    let policy = Retention {
        max_age_ms: Some(u64::MAX),
        ..Retention::default()
    };
    assert!(!policy.must_trim(1, 0, i64::MIN, i64::MAX));
}

/// Durability names in config are lowercase, and other spellings are refused.
#[test]
fn durability_deserialises_from_lowercase() {
    assert_eq!(
        serde_json::from_str::<Durability>("\"synced\"").ok(),
        Some(Durability::Synced)
    );
    assert_eq!(
        serde_json::from_str::<Durability>("\"buffered\"").ok(),
        Some(Durability::Buffered)
    );
    assert!(serde_json::from_str::<Durability>("\"Synced\"").is_err());
}

/// A duplicate whose original was trimmed is appended again with a new seq.
#[test]
fn duplicate_after_trim_is_a_new_record() {
    crate::conformance::block_on(async {
        let storage = MemoryStorage::new();
        let new = || NewRecord {
            message_id: "m".to_string(),
            publisher: "alice".to_string(),
            payload: "p".to_string(),
            encoding: None,
            qos: 1,
            published_at_ms: 1,
        };
        assert_eq!(
            storage
                .append("t", new(), Durability::None)
                .await
                .map(|r| r.seq),
            Ok(1)
        );
        let keep_none = Retention {
            max_messages: Some(0),
            ..Retention::default()
        };
        assert_eq!(storage.trim("t", &keep_none, 2).await, Ok(1));
        assert_eq!(
            storage
                .append("t", new(), Durability::None)
                .await
                .map(|r| r.seq),
            Ok(2)
        );
        assert!(!storage.capabilities().persistent);
    });
}

/// Checks that a record written before payload encodings existed reads back
/// as text, and that an encoding is stored beside the payload.
#[test]
fn records_keep_their_payload_encoding() {
    let old = r#"{"topic":"t","seq":1,"message_id":"m","publisher":"p","payload":"hi","qos":1,"published_at_ms":5}"#;
    let record: Record = serde_json::from_str(old).expect("an old record");
    assert_eq!(record.encoding, None);
    assert_eq!(serde_json::to_string(&record).expect("serialise"), old);

    let mut encoded = record;
    encoded.encoding = Some(PayloadEncoding::Base64);
    let text = serde_json::to_string(&encoded).expect("serialise");
    assert!(text.contains(r#""encoding":"base64""#), "{text}");
    let back: Record = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, encoded);
}

/// Checks that a record's own JSON writer gives exactly what its `Serialize`
/// gives, for plain text, text that needs escaping, text outside ASCII and a
/// base64 payload.
#[test]
fn a_record_writes_the_same_json_as_serde() {
    let payloads = [
        "plain text",
        "",
        "quote \" backslash \\\\ newline \n tab \t bell \u{7}",
        "caf\u{e9} \u{1f600} \u{7f}",
    ];
    for (n, payload) in payloads.iter().enumerate() {
        for encoding in [None, Some(crate::PayloadEncoding::Base64)] {
            let record = crate::Record {
                topic: format!("orders/\"{n}\""),
                seq: u64::MAX - n as u64,
                message_id: "0b2c-id".to_string(),
                publisher: "alice".to_string(),
                payload: (*payload).to_string(),
                encoding,
                qos: 1,
                published_at_ms: -5 + n as i64,
            };
            let expected = serde_json::to_string(&record).expect("serialise");
            assert_eq!(record.to_json(), expected);
            let read: crate::Record = serde_json::from_str(&record.to_json()).expect("parse");
            assert_eq!(read, record);
        }
    }
}
