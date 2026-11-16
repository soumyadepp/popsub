//! `SessionTable`, the in-memory rules for sessions, subscriptions and cursors.
//!
//! Both built-in backends keep their sessions in one of these:
//! `MemoryStorage` directly, `FileStorage` rebuilt from its session log on
//! open. The methods are synchronous; the backends lock and wrap them.

use std::collections::HashMap;

use crate::record::Subscription;

/// One session's last activity and its cursors.
#[derive(Debug, Default, Clone)]
struct Session {
    /// When the session was last touched, 0 if never.
    last_seen_ms: i64,
    /// Maps each subscribed topic to its `acked_seq`.
    cursors: HashMap<String, u64>,
}

/// Every session's subscriptions and cursors.
#[derive(Debug, Default)]
pub(crate) struct SessionTable {
    /// Sessions by id.
    sessions: HashMap<String, Session>,
}

impl SessionTable {
    /// Lists a session's subscriptions.
    ///
    /// Args:
    ///     session_id: The session.
    ///
    /// Returns:
    ///     The subscriptions sorted by topic, empty for an unknown session.
    pub(crate) fn load(&self, session_id: &str) -> Vec<Subscription> {
        let Some(session) = self.sessions.get(session_id) else {
            return Vec::new();
        };
        let mut subscriptions: Vec<Subscription> = session
            .cursors
            .iter()
            .map(|(topic, &acked_seq)| Subscription {
                session_id: session_id.to_string(),
                topic: topic.clone(),
                acked_seq,
            })
            .collect();
        subscriptions.sort_by(|a, b| a.topic.cmp(&b.topic));
        subscriptions
    }

    /// Sets a subscription's cursor, creating the session and subscription if needed.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///     acked_seq: The cursor.
    pub(crate) fn subscribe(&mut self, session_id: &str, topic: &str, acked_seq: u64) {
        self.sessions
            .entry(session_id.to_string())
            .or_default()
            .cursors
            .insert(topic.to_string(), acked_seq);
    }

    /// Removes a subscription if it exists.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    pub(crate) fn unsubscribe(&mut self, session_id: &str, topic: &str) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.cursors.remove(topic);
        }
    }

    /// Raises a subscription's cursor if it exists and `seq` is higher.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///     seq: The new cursor.
    ///
    /// Returns:
    ///     Whether the cursor moved.
    pub(crate) fn ack(&mut self, session_id: &str, topic: &str, seq: u64) -> bool {
        if let Some(session) = self.sessions.get_mut(session_id)
            && let Some(cursor) = session.cursors.get_mut(topic)
            && seq > *cursor
        {
            *cursor = seq;
            return true;
        }
        false
    }

    /// Records a session's activity, creating it if needed. Times only move forward.
    ///
    /// Args:
    ///     session_id: The session.
    ///     at_ms: When it was active.
    pub(crate) fn touch(&mut self, session_id: &str, at_ms: i64) {
        let session = self.sessions.entry(session_id.to_string()).or_default();
        session.last_seen_ms = session.last_seen_ms.max(at_ms);
    }

    /// Removes sessions last touched before a time.
    ///
    /// Args:
    ///     before_ms: The cutoff.
    ///
    /// Returns:
    ///     How many sessions were removed.
    pub(crate) fn expire(&mut self, before_ms: i64) -> u64 {
        let before = self.sessions.len();
        self.sessions
            .retain(|_, session| session.last_seen_ms >= before_ms);
        before.saturating_sub(self.sessions.len()) as u64
    }

    /// Reads a subscription's cursor.
    ///
    /// Args:
    ///     session_id: The session.
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The cursor, or `None` when there is no such subscription.
    pub(crate) fn cursor(&self, session_id: &str, topic: &str) -> Option<u64> {
        self.sessions.get(session_id)?.cursors.get(topic).copied()
    }

    /// Counts the sessions `expire` would remove, without removing them.
    ///
    /// Args:
    ///     before_ms: The cutoff.
    ///
    /// Returns:
    ///     The count.
    pub(crate) fn count_expired(&self, before_ms: i64) -> u64 {
        self.sessions
            .values()
            .filter(|session| session.last_seen_ms < before_ms)
            .count() as u64
    }

    /// Lists every session's activity time and subscriptions, for writing a
    /// compacted session log.
    ///
    /// Returns:
    ///     Each session id with its last activity and subscriptions.
    pub(crate) fn snapshot(&self) -> Vec<(String, i64, Vec<Subscription>)> {
        self.sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.last_seen_ms, self.load(id)))
            .collect()
    }
}
