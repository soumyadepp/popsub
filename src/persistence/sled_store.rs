use chrono::Utc;
use serde::{Deserialize, Serialize};
use sled::Db;

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
        let topic_tree = self.db.open_tree(topic).unwrap();
        topic_tree
            .insert(msg.timestamp.to_be_bytes(), serialized)
            .unwrap();
    }

    pub fn load_messages(&self, topic: &str) -> Vec<StoredMessage> {
        self.cleanup_old_messages(topic);
        let topic_tree = self.db.open_tree(topic).unwrap();
        topic_tree
            .iter()
            .filter_map(|res| res.ok())
            .filter_map(|(_, val)| serde_json::from_slice(&val).ok())
            .collect()
    }

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
