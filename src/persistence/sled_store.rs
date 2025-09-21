use chrono::Utc;
use serde::{Deserialize, Serialize};
use sled::Db;
use uuid::Uuid;

/// Represents a message that has been published to a topic and stored for later replay.
///
/// This struct is used to serialize and deserialize messages for persistent storage,
/// enabling features like message history and replay for reconnecting clients.
///
/// # Fields
///
/// - `topic`: The name of the topic to which this message was published.
/// - `payload`: The actual content of the message, typically a JSON string.
/// - `timestamp`: The Unix timestamp (in milliseconds) indicating when the message was originally published.
///
/// # Example
///
/// ```rust
/// use popsub::persistence::sled_store::StoredMessage;
/// let msg = StoredMessage {
///     topic: "sensor_data".to_string(),
///     payload: "{\"temp\":22}".to_string(),
///     timestamp: 1_725_000_000,
/// };
/// ```
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredMessage {
    /// The topic to which the message belongs.
    pub topic: String,
    /// The payload of the message.
    pub payload: String,
    /// The timestamp when the message was published (in milliseconds).
    pub timestamp: i64,
}

/// Handles persistent storage of published messages for replay in a Pub/Sub system.
///
/// This struct manages an embedded `sled` database to store messages, providing
/// durability and the ability to replay messages to clients. It supports optional
/// time-to-live (TTL) for messages and limits on the number of messages retained per topic.
///
/// # Fields
///
/// - `db`: The underlying `sled::Db` instance used for key-value storage.
/// - `ttl_seconds`: An `Option<i64>` specifying the time-to-live for messages in seconds.
///   If `Some(value)`, messages older than `value` seconds will be considered expired.
///   If `None`, messages are retained indefinitely.
/// - `max_messages_per_topic`: An `Option<usize>` specifying the maximum number of messages
///   to retain per topic. If `Some(value)`, older messages will be pruned when the limit is exceeded.
///   If `None`, all messages for a topic are retained (subject to `ttl_seconds`).
///
/// # Example
///
/// ```rust
/// use sled::Config;
/// use popsub::persistence::sled_store::Persistence;
/// let persistence = Persistence::new("messages.db", Some(3600), Some(100));
/// // The persistence can now be used to store and retrieve messages.
/// ```
#[derive(Clone)]
pub struct Persistence {
    /// The `sled` database instance for storing messages.
    db: Db,
    /// Optional time-to-live for messages in seconds.
    ttl_seconds: Option<i64>,
    /// Optional maximum number of messages to retain per topic.
    max_messages_per_topic: Option<usize>,
}

impl Persistence {
    /// Creates a new `Persistence` instance with an optional TTL and max message limit.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the database directory (used by `sled`).
    /// * `ttl_seconds` - Optional time-to-live (in seconds) for messages. Messages older than this will be deleted during access.
    /// * `max_messages_per_topic` - Optional limit on how many messages to store per topic (not yet enforced in this code).
    ///
    /// # Panics
    ///
    /// Panics if the sled database fails to open.
    pub fn new(
        path: &str,
        ttl_seconds: Option<i64>,
        max_messages_per_topic: Option<usize>,
    ) -> Self {
        let db = sled::open(path).expect("Failed to open sled DB");
        Self {
            db,
            ttl_seconds,
            max_messages_per_topic,
        }
    }

    /// Stores a message in the persistence layer under the specified topic.
    ///
    /// # Arguments
    ///
    /// * `topic` - The topic to store the message under.
    /// * `payload` - The message payload (as a string).
    ///
    /// # Errors
    ///
    /// Logs and returns early if serialization fails.
    ///
    /// # Implementation Details
    ///
    /// - Messages are stored under their timestamp (as big-endian bytes) to maintain order.
    /// - Each topic is stored in a separate sled tree.
    pub fn store_message(&self, topic: &str, payload: &str) {
        let msg = StoredMessage {
            topic: topic.to_string(),
            payload: payload.to_string(),
            timestamp: Utc::now().timestamp_millis(),
        };

        let serialized = match serde_json::to_vec(&msg) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("Failed to serialize message: {e}");
                return;
            }
        };

        let topic_tree = match self.db.open_tree(topic) {
            Ok(tree) => tree,
            Err(e) => {
                eprintln!("Failed to open topic tree '{topic}': {e}");
                return;
            }
        };

        // Key: zero-padded millis timestamp + UUID for uniqueness and order
        let key = format!("{:020}_{}", msg.timestamp, Uuid::new_v4());

        if let Err(e) = topic_tree.insert(key.as_bytes(), serialized) {
            eprintln!("Failed to store message in topic '{topic}': {e}");
            return;
        }

        // Enforce max_messages_per_topic
        if let Some(max) = self.max_messages_per_topic {
            let total_messages = topic_tree.len();
            if total_messages > max {
                let excess = total_messages - max;

                let keys_to_delete: Vec<_> = topic_tree
                    .iter()
                    .take(excess) // oldest keys first
                    .filter_map(|entry| entry.ok().map(|(k, _)| k))
                    .collect();

                for key in keys_to_delete {
                    if let Err(e) = topic_tree.remove(key) {
                        eprintln!("Failed to remove old message from '{topic}': {e}");
                    }
                }
            }
        }
    }

    /// Loads all valid (non-expired) messages for a given topic.
    ///
    /// Automatically triggers cleanup for expired messages if TTL is configured.
    ///
    /// # Arguments
    ///
    /// * `topic` - The topic whose messages to retrieve.
    ///
    /// # Returns
    ///
    /// A vector of deserialized `StoredMessage`s.
    pub fn load_messages(&self, topic: &str) -> Vec<StoredMessage> {
        self.cleanup_old_messages(topic);
        let topic_tree = self.db.open_tree(topic).unwrap();

        topic_tree
            .iter()
            .filter_map(|res| res.ok())
            .filter_map(|(_, val)| serde_json::from_slice(&val).ok())
            .collect()
    }

    /// Removes messages older than the TTL from the topic's message store.
    ///
    /// This is a lazy cleanup strategy, invoked during message access.
    ///
    /// # Arguments
    ///
    /// * `topic` - The topic for which to clean up old messages.
    fn cleanup_old_messages(&self, topic: &str) {
        if let Some(ttl) = self.ttl_seconds {
            let now = Utc::now().timestamp_millis(); // match precision with store_message
            let expiry_time = now - (ttl * 1000); // convert TTL to millis

            let topic_tree = self.db.open_tree(topic).unwrap();
            let old_keys: Vec<_> = topic_tree
                .iter()
                .filter_map(|res| res.ok())
                .filter_map(|(key_bytes, _)| {
                    // convert to UTF-8 string
                    if let Ok(key_str) = std::str::from_utf8(&key_bytes) {
                        // key format: "00000000000012345678_uuid"
                        if let Some((ts_str, _)) = key_str.split_once('_')
                            && let Ok(ts) = ts_str.parse::<i64>()
                            && ts < expiry_time
                        {
                            return Some(key_bytes);
                        }
                    }
                    None
                })
                .collect();

            for key in old_keys {
                let _ = topic_tree.remove(key);
            }
        }
    }
}

impl std::fmt::Debug for Persistence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Persistence")
            .field("db", &"sled::Db")
            .finish()
    }
}

impl Default for Persistence {
    fn default() -> Self {
        Self::new("pubsub_db", Some(3600), Some(1000)) // TTL: 1 hour, Max: 1000 messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
