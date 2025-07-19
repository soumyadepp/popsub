use chrono::Utc;
use serde::{Deserialize, Serialize};
use sled::Db;

/// Represents a message that has been published to a topic and stored for later replay.
///
/// Useful for scenarios where subscribers reconnect and need to receive past messages.
///
/// # Fields
///
/// - `topic` - The name of the topic this message belongs to.
/// - `payload` - The actual content of the message.
/// - `timestamp` - Unix timestamp (in milliseconds or seconds) indicating when the message was published.
///
/// # Example
///
/// ```rust
/// let msg = StoredMessage {
///     topic: "sensor_data".to_string(),
///     payload: "{\"temp\":22}".to_string(),
///     timestamp: 1_725_000_000,
/// };
/// ```
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredMessage {
    pub topic: String,
    pub payload: String,
    pub timestamp: i64,
}

/// Handles persistent storage of published messages for replay in a Pub/Sub system.
///
/// Stores messages in a local database (e.g., `sled`), optionally applying TTL (time-to-live)
/// and per-topic message retention limits.
///
/// # Fields
///
/// - `db` - The underlying key-value store for message persistence.
/// - `ttl_seconds` - Optional expiration time (in seconds) for each stored message. If `None`, messages are kept indefinitely.
/// - `max_messages_per_topic` - Optional limit on the number of messages retained per topic. If `None`, all messages are retained.
///
/// # Example
///
/// ```rust
/// let persistence = Persistence {
///     db: sled::open("messages.db").unwrap(),
///     ttl_seconds: Some(3600),
///     max_messages_per_topic: Some(100),
/// };
/// ```
#[derive(Clone)]
pub struct Persistence {
    db: Db,
    ttl_seconds: Option<i64>,
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
            timestamp: Utc::now().timestamp(),
        };

        let serialized = match serde_json::to_vec(&msg) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("Failed to serialize message: {:?}", e);
                return;
            }
        };

        let topic_tree = match self.db.open_tree(topic) {
            Ok(tree) => tree,
            Err(e) => {
                eprintln!("Failed to open topic tree '{}': {:?}", topic, e);
                return;
            }
        };

        // Insert new message
        if let Err(e) = topic_tree.insert(msg.timestamp.to_be_bytes(), serialized) {
            eprintln!("Failed to store message in topic '{}': {:?}", topic, e);
            return;
        }

        // Enforce max_messages_per_topic
        if let Some(max) = self.max_messages_per_topic {
            let total_messages = topic_tree.len();

            if total_messages > max {
                let excess = total_messages - max;

                let keys_to_delete: Vec<_> = topic_tree
                    .iter()
                    .take(excess)
                    .filter_map(|entry| entry.ok().map(|(k, _)| k))
                    .collect();

                for key in keys_to_delete {
                    if let Err(e) = topic_tree.remove(key) {
                        eprintln!("Failed to remove old message from '{}': {:?}", topic, e);
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
            let now = Utc::now().timestamp();
            let expiry_time = now - ttl;

            let topic_tree = self.db.open_tree(topic).unwrap();
            let old_keys: Vec<_> = topic_tree
                .iter()
                .filter_map(|res| res.ok())
                .filter_map(|(key, _)| {
                    if key.len() == 8 {
                        let ts = i64::from_be_bytes(key[..].try_into().unwrap());
                        if ts < expiry_time { Some(key) } else { None }
                    } else {
                        None
                    }
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
