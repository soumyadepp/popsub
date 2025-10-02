#[cfg(test)]
mod persistence_tests {
    use crate::Persistence;
    use crate::sled_store::StoredMessage;

    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::tempdir;

    fn create_test_persistence(ttl: Option<i64>, max: Option<usize>) -> Persistence {
        let dir = tempdir().unwrap();
        Persistence::new(dir.path().to_str().unwrap(), ttl, max)
    }

    #[test]
    fn test_store_and_load_message() {
        let persistence = create_test_persistence(None, None);
        let topic = "test_topic";

        persistence.store_message(topic, "hello");
        let messages = persistence.load_messages(topic);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, "hello");
        assert_eq!(messages[0].topic, topic);
    }

    #[test]
    fn test_ttl_removes_old_messages() {
        let persistence = create_test_persistence(Some(1), None);
        let topic = "ttl_test";

        persistence.store_message(topic, "msg1");
        sleep(Duration::from_secs(2)); // Wait so the TTL expires
        let messages = persistence.load_messages(topic);

        assert!(messages.is_empty(), "Messages should be expired");
    }

    #[test]
    fn test_max_messages_limit() {
        let persistence = create_test_persistence(Some(1000), Some(3));
        let topic = "max_limit_test";

        for i in 0..5 {
            let msg = format!("msg{i}");
            persistence.store_message(topic, &msg);
            std::thread::sleep(std::time::Duration::from_millis(2)); // ensure timestamp uniqueness
        }

        let messages = persistence.load_messages(topic);

        // Collect payloads and assert length
        let mut payloads: Vec<_> = messages.iter().map(|m| m.payload.clone()).collect();
        payloads.sort(); // Sort if ordering isn't guaranteed

        let expected = vec!["msg2", "msg3", "msg4"];
        assert_eq!(payloads.len(), 3);
        assert_eq!(payloads, expected);
    }

    #[test]
    fn test_empty_topic_returns_empty_vec() {
        let persistence = create_test_persistence(None, None);
        let messages = persistence.load_messages("nonexistent_topic");
        assert!(messages.is_empty());
    }

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

    #[test]
    fn test_cleanup_old_messages_no_ttl() {
        let persistence = create_test_persistence(None, None);
        let topic = "no_ttl_test";

        persistence.store_message(topic, "msg1");
        sleep(Duration::from_secs(2)); // Wait
        let messages = persistence.load_messages(topic);

        assert_eq!(messages.len(), 1, "Message should not be expired");
    }
}
