//! Topic permissions and permission checking
//!
//! This module provides topic-level permission definitions and wildcard matching.

use serde::{Deserialize, Serialize};

/// Permission configuration for a specific topic or topic pattern.
///
/// Supports MQTT-style wildcards:
/// - `+` matches a single level (e.g., `sensors/+/temperature`)
/// - `#` matches multiple levels (e.g., `chat/#`)
/// - `*` is also supported as an alias for `#` (e.g., `chat/*`)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopicPermission {
    /// Topic pattern (may include wildcards).
    pub pattern: String,
    /// Whether the user can subscribe to matching topics.
    pub can_subscribe: bool,
    /// Whether the user can publish to matching topics.
    pub can_publish: bool,
}

impl TopicPermission {
    /// Create a new topic permission.
    pub fn new(pattern: impl Into<String>, can_subscribe: bool, can_publish: bool) -> Self {
        Self {
            pattern: pattern.into(),
            can_subscribe,
            can_publish,
        }
    }

    /// Create a read-only permission for a topic.
    pub fn read_only(pattern: impl Into<String>) -> Self {
        Self::new(pattern, true, false)
    }

    /// Create a write-only permission for a topic.
    pub fn write_only(pattern: impl Into<String>) -> Self {
        Self::new(pattern, false, true)
    }

    /// Create a full access permission for a topic.
    pub fn full_access(pattern: impl Into<String>) -> Self {
        Self::new(pattern, true, true)
    }

    /// Check if this permission matches the given topic.
    pub fn matches(&self, topic: &str) -> bool {
        topic_matches_pattern(&self.pattern, topic)
    }
}

/// Check if a topic matches a pattern with wildcards.
///
/// Wildcard rules:
/// - `#` or `*` at the end matches any remaining levels
/// - `+` matches exactly one level
/// - Exact match otherwise
///
/// # Examples
///
/// ```
/// use popsub_auth::permission::topic_matches_pattern;
///
/// assert!(topic_matches_pattern("chat/#", "chat/room1"));
/// assert!(topic_matches_pattern("chat/#", "chat/room1/messages"));
/// assert!(topic_matches_pattern("sensors/+/temp", "sensors/living_room/temp"));
/// assert!(!topic_matches_pattern("sensors/+/temp", "sensors/living_room/humidity"));
/// assert!(!topic_matches_pattern("chat/room1", "chat/room2"));
/// ```
pub fn topic_matches_pattern(pattern: &str, topic: &str) -> bool {
    // Handle exact match
    if pattern == topic {
        return true;
    }

    // Handle multi-level wildcards (# or *)
    if pattern == "#" || pattern == "*" {
        return true;
    }

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    let mut pattern_idx = 0;
    let mut topic_idx = 0;

    while pattern_idx < pattern_parts.len() && topic_idx < topic_parts.len() {
        let pattern_part = pattern_parts[pattern_idx];

        match pattern_part {
            // Multi-level wildcard - matches everything remaining
            "#" | "*" => return true,
            // Single-level wildcard - matches exactly one level
            "+" => {
                pattern_idx += 1;
                topic_idx += 1;
            }
            // Exact match required
            _ => {
                if pattern_part != topic_parts[topic_idx] {
                    return false;
                }
                pattern_idx += 1;
                topic_idx += 1;
            }
        }
    }

    // Check if we've consumed both pattern and topic completely
    // or if the last pattern part is a multi-level wildcard
    if pattern_idx == pattern_parts.len() && topic_idx == topic_parts.len() {
        return true;
    }

    // Pattern ended with wildcard that wasn't processed
    if pattern_idx < pattern_parts.len() {
        let remaining = &pattern_parts[pattern_idx..];
        if remaining.len() == 1 && (remaining[0] == "#" || remaining[0] == "*") {
            return true;
        }
    }

    false
}

/// Permission checker that evaluates access based on user roles and topic patterns.
pub struct PermissionChecker;

impl PermissionChecker {
    /// Check if the given permissions allow subscribing to the topic.
    pub fn can_subscribe(permissions: &[TopicPermission], topic: &str) -> bool {
        permissions
            .iter()
            .any(|p| p.can_subscribe && p.matches(topic))
    }

    /// Check if the given permissions allow publishing to the topic.
    pub fn can_publish(permissions: &[TopicPermission], topic: &str) -> bool {
        permissions
            .iter()
            .any(|p| p.can_publish && p.matches(topic))
    }

    /// Check if a list of topic patterns (strings) allows subscribing to the topic.
    pub fn can_subscribe_readonly(allowed_topics: &[String], topic: &str) -> bool {
        allowed_topics
            .iter()
            .any(|pattern| topic_matches_pattern(pattern, topic))
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(topic_matches_pattern("chat/room1", "chat/room1"));
        assert!(!topic_matches_pattern("chat/room1", "chat/room2"));
    }

    #[test]
    fn test_multi_level_wildcard_hash() {
        assert!(topic_matches_pattern("chat/#", "chat"));
        assert!(topic_matches_pattern("chat/#", "chat/room1"));
        assert!(topic_matches_pattern("chat/#", "chat/room1/messages"));
        assert!(!topic_matches_pattern("chat/#", "other/room1"));
    }

    #[test]
    fn test_multi_level_wildcard_star() {
        assert!(topic_matches_pattern("chat/*", "chat/room1"));
        assert!(topic_matches_pattern("chat/*", "chat/room1/messages"));
        assert!(!topic_matches_pattern("chat/*", "other/room1"));
    }

    #[test]
    fn test_single_level_wildcard() {
        assert!(topic_matches_pattern(
            "sensors/+/temp",
            "sensors/living_room/temp"
        ));
        assert!(topic_matches_pattern(
            "sensors/+/temp",
            "sensors/bedroom/temp"
        ));
        assert!(!topic_matches_pattern(
            "sensors/+/temp",
            "sensors/living_room/humidity"
        ));
        assert!(!topic_matches_pattern("sensors/+/temp", "sensors/a/b/temp"));
    }

    #[test]
    fn test_global_wildcard() {
        assert!(topic_matches_pattern("#", "anything"));
        assert!(topic_matches_pattern("#", "any/thing/at/all"));
        assert!(topic_matches_pattern("*", "anything"));
    }

    #[test]
    fn test_permission_checker() {
        let permissions = vec![
            TopicPermission::new("chat/#", true, true),
            TopicPermission::new("sensors/+/temp", true, false),
        ];

        assert!(PermissionChecker::can_subscribe(&permissions, "chat/room1"));
        assert!(PermissionChecker::can_publish(&permissions, "chat/room1"));
        assert!(PermissionChecker::can_subscribe(
            &permissions,
            "sensors/living_room/temp"
        ));
        assert!(!PermissionChecker::can_publish(
            &permissions,
            "sensors/living_room/temp"
        ));
        assert!(!PermissionChecker::can_subscribe(
            &permissions,
            "other/topic"
        ));
    }
}
