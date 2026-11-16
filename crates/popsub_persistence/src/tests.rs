//! Unit tests for `Persistence`: storing, loading, TTL expiry, the per-topic
//! cap and the `StoredMessage` encoding.

#[cfg(test)]
mod persistence_tests {
    use crate::Persistence;
    use crate::sled_store::StoredMessage;

    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::tempdir;

    /// Opens a `Persistence` in a new temporary directory with the given limits.
    ///
    /// The `TempDir` guard is dropped when this function returns, so the
    /// directory is deleted while the database is still open; the tests rely
    /// on `sled` keeping its open files usable.
    ///
    /// Args:
    ///     ttl: TTL in seconds, or `None` for no expiry.
    ///     max: Per-topic message cap, or `None` for no cap.
    ///
    /// Returns:
    ///     A handle to the new database.
    fn create_test_persistence(
        ttl: Option<i64>,
        max: Option<usize>,
    ) -> (Persistence, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let persistence =
            Persistence::new(dir.path().to_str().expect("path to_str failed"), ttl, max)
                .expect("Failed to create persistence");
        (persistence, dir)
    }

    /// Checks that a stored message loads back with the same topic and payload.
    #[test]
    fn test_store_and_load_message() {
        let (persistence, _dir) = create_test_persistence(None, None);
        let topic = "test_topic";

        let _ = persistence.store_message(topic, "hello");
        let messages = persistence
            .load_messages(topic)
            .expect("Failed to load messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, "hello");
        assert_eq!(messages[0].topic, topic);
    }

    /// Checks that a message older than a one-second TTL is gone after two seconds.
    #[test]
    fn test_ttl_removes_old_messages() {
        let (persistence, _dir) = create_test_persistence(Some(1), None);
        let topic = "ttl_test";

        let _ = persistence.store_message(topic, "msg1");
        sleep(Duration::from_secs(2));
        let messages = persistence.load_messages(topic);
        assert!(
            Result::unwrap(messages).is_empty(),
            "Messages should be expired"
        );
    }

    /// Checks that a cap of three keeps only the three newest of five messages, in order.
    #[test]
    fn test_max_messages_limit() {
        let (persistence, _dir) = create_test_persistence(Some(1000), Some(3));
        let topic = "max_limit_test";

        for i in 0..5 {
            let msg = format!("msg{i}");
            let _ = persistence.store_message(topic, &msg);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let messages = persistence
            .load_messages(topic)
            .expect("Failed to load messages");
        assert_eq!(messages.len(), 3, "Should only keep max 3 messages");
        assert_eq!(messages[0].payload, "msg2");
        assert_eq!(messages[1].payload, "msg3");
        assert_eq!(messages[2].payload, "msg4");
    }

    /// Checks that loading a topic with no messages returns an empty list.
    #[test]
    fn test_empty_topic_returns_empty_vec() {
        let (persistence, _dir) = create_test_persistence(None, None);
        let messages = persistence.load_messages("nonexistent_topic");
        assert!(
            messages.unwrap().is_empty(),
            "Expected no messages for nonexistent topic"
        );
    }

    /// Checks that a `StoredMessage` survives a JSON encode and decode unchanged.
    #[test]
    fn test_serialization_roundtrip() {
        let msg = StoredMessage {
            topic: "roundtrip".into(),
            payload: "{\"key\":42}".into(),
            timestamp: 1725000000,
        };

        let data = serde_json::to_vec(&msg).unwrap();
        let parsed: StoredMessage = serde_json::from_slice(&data).unwrap();

        assert_eq!(msg.topic, parsed.topic);
        assert_eq!(msg.payload, parsed.payload);
        assert_eq!(msg.timestamp, parsed.timestamp);
    }

    /// Checks that without a TTL a stored message is never expired by a load.
    #[test]
    fn test_cleanup_old_messages_no_ttl() {
        let (persistence, _dir) = create_test_persistence(None, None);
        let topic = "no_ttl_test";

        let _ = persistence.store_message(topic, "msg1");
        let messages = persistence.load_messages(topic);

        assert_eq!(
            Result::unwrap(messages).len(),
            1,
            "Message should not be expired"
        );
    }
}
