use std::collections::{HashMap, HashSet};

pub type SubscriberId = String;

#[derive(Debug, Default)]
pub struct Topic {
    pub name: String,
    pub subscribers: HashSet<SubscriberId>,
}

impl Topic {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subscribers: HashSet::new(),
        }
    }

    pub fn subscribe(&mut self, id: SubscriberId) {
        self.subscribers.insert(id);
    }

    pub fn unsubscribe(&mut self, id: &SubscriberId) {
        self.subscribers.remove(id);
    }
}
