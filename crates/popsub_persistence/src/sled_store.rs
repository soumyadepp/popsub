//! Message storage backed by `sled`, one tree per topic.
//!
//! Each entry's key is `{timestamp_ms:020}_{uuid}`, so iterating a tree
//! yields messages oldest first, and the value is a JSON `StoredMessage`.
//! Two optional limits apply: `ttl_seconds` removes older messages, but only
//! when the topic is next loaded, and `max_messages_per_topic` removes the
//! oldest messages as soon as a store pushes the topic over the cap.

use chrono::Utc;
use popsub_utils::error::{PopSubError, Result};
use serde::{Deserialize, Serialize};
use sled::Db;
use uuid::Uuid;

/// A published message as kept on disk.
///
/// The message id and QoS level are not stored, so replayed messages can't
/// be matched to the original delivery.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredMessage {
    /// Topic the message was published to.
    pub topic: String,
    /// Message body as sent by the publisher.
    pub payload: String,
    /// When the message was stored, in milliseconds since the Unix epoch.
    pub timestamp: i64,
}

/// Handle to the message database and its retention limits.
///
/// Clones share the same underlying `sled` database.
#[derive(Clone)]
pub struct Persistence {
    /// The open `sled` database; each topic is a tree inside it.
    db: Db,
    /// Age in seconds after which a message is removed on load, or no limit.
    ttl_seconds: Option<i64>,
    /// Most messages kept per topic, or no limit.
    max_messages_per_topic: Option<usize>,
}

impl Persistence {
    /// Opens or creates a `sled` database at `path` with the given limits.
    ///
    /// `sled` locks its directory, so a second process or a second
    /// `Persistence::new` on the same path fails.
    ///
    /// Args:
    ///     path: Directory of the database.
    ///     ttl_seconds: Age in seconds after which messages are removed on
    ///         load, or `None` to keep them indefinitely.
    ///     max_messages_per_topic: Most messages to keep per topic, or
    ///         `None` for no cap.
    ///
    /// Returns:
    ///     A handle to the opened database.
    ///
    /// Raises:
    ///     PopSubError::Persistence: When the database can't be opened, for
    ///         example because another process holds the lock.
    pub fn new(
        path: &str,
        ttl_seconds: Option<i64>,
        max_messages_per_topic: Option<usize>,
    ) -> Result<Self> {
        let db = sled::open(path).map_err(|e| PopSubError::Persistence(e.to_string()))?;
        Ok(Self {
            db,
            ttl_seconds,
            max_messages_per_topic,
        })
    }

    /// Stores a message in its topic's tree, stamped with the current time.
    ///
    /// If the topic then holds more than `max_messages_per_topic` entries,
    /// the oldest are removed until it is back at the cap. The insert and the
    /// trim are separate operations, not one transaction.
    ///
    /// Args:
    ///     topic: Topic the message belongs to; also the tree name.
    ///     payload: Message body to store.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the message can't be encoded as JSON.
    ///     PopSubError::Persistence: When the tree can't be opened, or an insert
    ///         or removal fails.
    pub fn store_message(&self, topic: &str, payload: &str) -> Result<()> {
        let msg = StoredMessage {
            topic: topic.to_string(),
            payload: payload.to_string(),
            timestamp: Utc::now().timestamp_millis(),
        };

        let serialized = serde_json::to_vec(&msg)?;

        let topic_tree = self
            .db
            .open_tree(topic)
            .map_err(|e| PopSubError::Persistence(e.to_string()))?;

        let key = format!("{:020}_{}", msg.timestamp, Uuid::new_v4());

        topic_tree
            .insert(key.as_bytes(), serialized)
            .map_err(|e| PopSubError::Persistence(e.to_string()))?;

        if let Some(max) = self.max_messages_per_topic
            && topic_tree.len() > max
        {
            let total_messages = topic_tree.len();
            let excess = total_messages - max;

            let keys_to_delete: Vec<_> = topic_tree
                .iter()
                .take(excess)
                .filter_map(|entry| entry.ok().map(|(k, _)| k))
                .collect();

            for key in keys_to_delete {
                topic_tree
                    .remove(key)
                    .map_err(|e| PopSubError::Persistence(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Loads a topic's messages, oldest first, after removing expired ones.
    ///
    /// Entries that can't be read or decoded are skipped without an error.
    ///
    /// Args:
    ///     topic: Topic whose messages to load.
    ///
    /// Returns:
    ///     The stored messages that are still within the TTL, empty for a
    ///     topic with none.
    ///
    /// Raises:
    ///     PopSubError::Persistence: When the topic's tree can't be opened.
    pub fn load_messages(&self, topic: &str) -> Result<Vec<StoredMessage>> {
        self.cleanup_old_messages(topic)?;
        let topic_tree = self
            .db
            .open_tree(topic)
            .map_err(|e| PopSubError::Persistence(e.to_string()))?;

        Ok(topic_tree
            .iter()
            .filter_map(|res| res.ok())
            .filter_map(|(_, val)| serde_json::from_slice(&val).ok())
            .collect())
    }

    /// Removes a topic's messages older than the TTL, judged by the
    /// timestamp in each key. Does nothing when no TTL is set.
    ///
    /// Keys that don't parse as `{timestamp}_{uuid}` are kept, and failed
    /// removals are ignored.
    ///
    /// Args:
    ///     topic: Topic whose expired messages to remove.
    ///
    /// Raises:
    ///     PopSubError::Persistence: When the topic's tree can't be opened.
    fn cleanup_old_messages(&self, topic: &str) -> Result<()> {
        if let Some(ttl) = self.ttl_seconds {
            let now = Utc::now().timestamp_millis();
            let expiry_time = now - (ttl * 1000);

            let topic_tree = self
                .db
                .open_tree(topic)
                .map_err(|e| PopSubError::Persistence(e.to_string()))?;
            let old_keys: Vec<_> = topic_tree
                .iter()
                .filter_map(|res| res.ok())
                .filter_map(|(key_bytes, _)| {
                    if let Ok(key_str) = std::str::from_utf8(&key_bytes)
                        && let Some((ts_str, _)) = key_str.split_once('_')
                        && let Ok(ts) = ts_str.parse::<i64>()
                        && ts < expiry_time
                    {
                        return Some(key_bytes);
                    }
                    None
                })
                .collect();

            for key in old_keys {
                let _ = topic_tree.remove(key);
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for Persistence {
    /// Writes `Persistence { db: "sled::Db" }`, leaving out the limits.
    ///
    /// Args:
    ///     f: Formatter to write to.
    ///
    /// Returns:
    ///     The formatter's result.
    ///
    /// Raises:
    ///     std::fmt::Error: When the formatter fails to write.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Persistence")
            .field("db", &"sled::Db")
            .finish()
    }
}

impl Default for Persistence {
    /// Opens `pubsub_db` in the working directory with a 3600 second TTL and
    /// a cap of 1000 messages per topic.
    ///
    /// Panics if the database can't be opened, for example when another
    /// process holds its lock.
    ///
    /// Returns:
    ///     A handle to the default database.
    fn default() -> Self {
        Self::new("pubsub_db", Some(3600), Some(1000))
            .expect("Failed to create default persistence")
    }
}
