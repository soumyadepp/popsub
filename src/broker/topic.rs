use std::collections::{HashMap, HashSet};

pub type SubscriberId = String;

/// Represents a topic in the broker system
/// Contains a name and a set of subscribers
/// The topic is identified by its name and can have multiple subscribers
/// Subscribers can subscribe to or unsubscribe from the topic
/// This struct is used to manage the subscribers of a topic
/// It allows for adding and removing subscribers, enabling the pub/sub functionality
/// The `Topic` struct is essential for the broker's operation, as it maintains the state
/// of each topic and its subscribers.
#[derive(Debug, Default)]
pub struct Topic {
    pub name: String,
    pub subscribers: HashSet<SubscriberId>,
}

impl Topic {
    /// Creates a new instance of the Topic with the given name
    /// Initializes an empty set of subscribers
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subscribers: HashSet::new(),
        }
    }

    /// Subscribes a new subscriber to the topic
    /// Adds the subscriber's ID to the set of subscribers
    /// If the subscriber is already subscribed, it has no effect
    pub fn subscribe(&mut self, id: SubscriberId) {
        self.subscribers.insert(id);
    }

    /// Unsubscribes a subscriber from the topic
    /// Removes the subscriber's ID from the set of subscribers
    /// If the subscriber is not subscribed, it has no effect
    pub fn unsubscribe(&mut self, id: &SubscriberId) {
        self.subscribers.remove(id);
    }
}
