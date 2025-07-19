use std::collections::HashSet;

/// Unique identifier for a subscriber (e.g., a WebSocket client ID).
pub type SubscriberId = String;

/// Represents a Pub/Sub topic, which maintains a list of subscribers.
///
/// Each `Topic` has a name and tracks all subscribed clients by their `SubscriberId`.
#[derive(Debug, Default)]
pub struct Topic {
    /// Name of the topic.
    pub name: String,

    /// Set of subscriber IDs subscribed to this topic.
    pub subscribers: HashSet<SubscriberId>,
}

impl Topic {
    /// Creates a new `Topic` with the given name and no subscribers.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the topic to initialize.
    ///
    /// # Example
    ///
    /// ```rust
    /// let topic = Topic::new("news");
    /// ```
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subscribers: HashSet::new(),
        }
    }

    /// Subscribes a client (by ID) to the topic.
    ///
    /// # Arguments
    ///
    /// * `id` - The subscriber's unique ID.
    ///
    /// # Example
    ///
    /// ```rust
    /// topic.subscribe("client123".to_string());
    /// ```
    pub fn subscribe(&mut self, id: SubscriberId) {
        self.subscribers.insert(id);
    }

    /// Unsubscribes a client from the topic.
    ///
    /// # Arguments
    ///
    /// * `id` - The subscriber ID to remove.
    pub fn unsubscribe(&mut self, id: &SubscriberId) {
        self.subscribers.remove(id);
    }
}
