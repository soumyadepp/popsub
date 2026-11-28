//! Per-account topic counts, for the limit on how many topics one account
//! may create (`limits.max_topics_per_user`).
//!
//! A topic belongs to the account whose publish created it. Combined with
//! each topic's retention limits, a cap on how many topics an account
//! creates bounds the disk and memory one account can take. The counts are
//! built from the storage backend's creators the first time they are
//! needed, so they hold across restarts, and updated as topics are claimed.

use std::collections::HashMap;

/// What claiming a topic for a publish decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicClaim {
    /// The topic already exists; publishing to it creates nothing.
    Existing,
    /// The topic is new and now counts against the account.
    Created,
    /// The topic is new and the account has already created `limit` topics.
    LimitReached {
        /// The limit the account is at.
        limit: usize,
    },
}

/// Every known topic's creator, and how many topics each account created.
#[derive(Debug, Default)]
pub(crate) struct TopicOwners {
    /// Every topic that exists or has been claimed, with the account that
    /// created it, or `None` when the server itself did or it isn't known.
    owner_of: HashMap<String, Option<String>>,
    /// How many topics each account created.
    counts: HashMap<String, usize>,
}

impl TopicOwners {
    /// Builds the counts from what the storage backend knows.
    ///
    /// Args:
    ///     topics: Every topic with a log.
    ///     creators: `(topic, creator)` for the topics whose creator is known.
    ///
    /// Returns:
    ///     The counts.
    pub(crate) fn from_storage(topics: Vec<String>, creators: Vec<(String, String)>) -> Self {
        let mut owners = Self::default();
        for topic in topics {
            owners.owner_of.insert(topic, None);
        }
        for (topic, creator) in creators {
            *owners.counts.entry(creator.clone()).or_default() += 1;
            owners.owner_of.insert(topic, Some(creator));
        }
        owners
    }

    /// Claims a topic for a publish by `username`.
    ///
    /// An existing topic is always allowed. A new one is counted against
    /// the account, unless the account has already created `limit` topics.
    ///
    /// Args:
    ///     topic: The topic being published to.
    ///     username: The publishing account.
    ///     limit: The most topics the account may create, or `None` for no limit.
    ///
    /// Returns:
    ///     What was decided.
    pub(crate) fn claim(
        &mut self,
        topic: &str,
        username: &str,
        limit: Option<usize>,
    ) -> TopicClaim {
        if self.owner_of.contains_key(topic) {
            return TopicClaim::Existing;
        }
        let count = self.counts.get(username).copied().unwrap_or(0);
        if let Some(limit) = limit
            && count >= limit
        {
            return TopicClaim::LimitReached { limit };
        }
        self.counts.insert(username.to_string(), count + 1);
        self.owner_of
            .insert(topic.to_string(), Some(username.to_string()));
        TopicClaim::Created
    }

    /// Records a topic the server created itself, such as the dead letter
    /// topic, without counting it against any account.
    ///
    /// Args:
    ///     topic: The topic.
    pub(crate) fn note(&mut self, topic: &str) {
        self.owner_of.entry(topic.to_string()).or_insert(None);
    }

    /// How many topics an account has created.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Returns:
    ///     The count.
    pub(crate) fn count(&self, username: &str) -> usize {
        self.counts.get(username).copied().unwrap_or(0)
    }
}
