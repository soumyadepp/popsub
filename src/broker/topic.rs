use std::collections::HashSet;

/// Unique identifier for a subscriber (e.g., a WebSocket client ID).
pub type SubscriberId = String;

/// Represents a Pub/Sub topic, which maintains a list of subscribers.
///
/// A `Topic` is a named channel that clients can subscribe to in order to receive messages.
/// It keeps track of all subscribers interested in the topic, ensuring that published
/// messages are delivered to the correct clients.
#[derive(Debug, Default)]
pub struct Topic {
    /// The unique name of the topic (e.g., "sports", "news").
    pub name: String,

    /// A set of unique `SubscriberId`s for all clients subscribed to this topic.
    /// Using a `HashSet` prevents duplicate subscriptions and provides efficient
    /// addition and removal of subscribers.
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
    /// use popsub::broker::topic::Topic;
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
    /// use popsub::broker::topic::Topic;
    /// let mut topic = Topic::new("news");
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