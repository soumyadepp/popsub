//! Topic management
//!
//! A `Topic` holds the list of subscriber IDs for a particular topic name.
//! The topic implementation is intentionally minimal — subscriptions are stored
//! as a `HashSet` of `SubscriberId` and duplicate subscriptions are a no-op.
//!
//! Concurrency note: callers must synchronize access to `Topic` (for example
//! via the broker lock) when modifying subscriptions.

use std::collections::HashSet;

pub type SubscriberId = String;

#[derive(Debug, Default)]
pub struct Topic {
    pub name: String,
    pub subscribers: HashSet<SubscriberId>,
}

impl Topic {
    /// Create a new topic with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subscribers: HashSet::new(),
        }
    }

    /// Add a subscriber to the topic. Duplicate adds are ignored.
    pub fn subscribe(&mut self, id: SubscriberId) {
        self.subscribers.insert(id);
    }

    /// Remove a subscriber from the topic.
    pub fn unsubscribe(&mut self, id: &SubscriberId) {
        self.subscribers.remove(id);
    }
}
