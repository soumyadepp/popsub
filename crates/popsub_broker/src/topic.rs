//! The `Topic` type: a topic name and the set of client ids subscribed to it.
//!
//! `Topic` has no locking of its own. The broker owns every topic and
//! callers reach them through the broker's mutex.

use std::collections::HashSet;

/// Id of a subscribed client, the same string as `Client::id`.
pub type SubscriberId = String;

/// A named topic and the ids of the clients subscribed to it.
#[derive(Debug, Default)]
pub struct Topic {
    /// Topic name, the key the broker routes on.
    pub name: String,
    /// Ids of subscribed clients. A set, so subscribing twice has no effect.
    pub subscribers: HashSet<SubscriberId>,
}

impl Topic {
    /// Creates a topic with no subscribers.
    ///
    /// Args:
    ///     name: The topic name.
    ///
    /// Returns:
    ///     The new, empty topic.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subscribers: HashSet::new(),
        }
    }

    /// Adds a subscriber to the topic. Adding an existing subscriber does nothing.
    ///
    /// Args:
    ///     id: The id of the client to add.
    pub fn subscribe(&mut self, id: SubscriberId) {
        self.subscribers.insert(id);
    }

    /// Removes a subscriber from the topic. Removing an unknown id does nothing.
    ///
    /// Args:
    ///     id: The id of the client to remove.
    pub fn unsubscribe(&mut self, id: &SubscriberId) {
        self.subscribers.remove(id);
    }
}
