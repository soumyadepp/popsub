//! Persistence layer backed by `sled`
//!
//! This provides a small persistence abstraction that stores recent messages
//! per-topic in a `sled` tree. Each message key is prefixed with a timestamp
//! to allow chronological scans and TTL-based cleanup.
//!
//! Configuration options supported:
//! - `ttl_seconds`: optional time-to-live for messages (older messages will be
//!   removed during load)
//! - `max_messages_per_topic`: optional cap to limit storage per topic; when
//!   exceeded oldest messages are removed.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sled::Db;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredMessage {
    pub topic: String,
    pub payload: String,
    pub timestamp: i64,
}

#[derive(Clone)]
pub struct Persistence {
    db: Db,
    ttl_seconds: Option<i64>,
    max_messages_per_topic: Option<usize>,
}

impl Persistence {
    /// Open or create a sled database at `path` with the given policy.
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

    /// Store a message in the topic's tree. Keys are timestamp-prefixed so
    /// iteration yields messages in chronological order.
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

        let key = format!("{:020}_{}", msg.timestamp, Uuid::new_v4());

        if let Err(e) = topic_tree.insert(key.as_bytes(), serialized) {
            eprintln!("Failed to store message in topic '{topic}': {e}");
            return;
        }

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
                        eprintln!("Failed to remove old message from '{topic}': {e}");
                    }
                }
            }
        }
    }

    /// Load messages for a topic honoring TTL and retention policy.
    pub fn load_messages(&self, topic: &str) -> Vec<StoredMessage> {
        self.cleanup_old_messages(topic);
        let topic_tree = self.db.open_tree(topic).unwrap();

        topic_tree
            .iter()
            .filter_map(|res| res.ok())
            .filter_map(|(_, val)| serde_json::from_slice(&val).ok())
            .collect()
    }

    /// Remove messages older than the TTL for a single topic.
    fn cleanup_old_messages(&self, topic: &str) {
        if let Some(ttl) = self.ttl_seconds {
            let now = Utc::now().timestamp_millis();
            let expiry_time = now - (ttl * 1000);

            let topic_tree = self.db.open_tree(topic).unwrap();
            let old_keys: Vec<_> = topic_tree
                .iter()
                .filter_map(|res| res.ok())
                .filter_map(|(key_bytes, _)| {
                    if let Ok(key_str) = std::str::from_utf8(&key_bytes) {
                        if let Some((ts_str, _)) = key_str.split_once('_') {
                            if let Ok(ts) = ts_str.parse::<i64>() {
                                if ts < expiry_time {
                                    return Some(key_bytes);
                                }
                            }
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
        Self::new("pubsub_db", Some(3600), Some(1000))
    }
}
