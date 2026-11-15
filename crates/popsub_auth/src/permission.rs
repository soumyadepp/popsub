//! Topic permission patterns, MQTT-style wildcard matching, and the checks built on them.
//!
//! `AuthService` uses `PermissionChecker` to evaluate `Role::User` and
//! `Role::ReadOnly` topic lists. Admins skip these checks.

use serde::{Deserialize, Serialize};

/// A topic pattern with separate subscribe and publish flags.
///
/// Patterns use MQTT-style wildcards: `+` matches exactly one level (as in
/// `sensors/+/temperature`) and a trailing `#` matches the parent level and
/// any number of levels below it (as in `chat/#`). `*` has no special meaning
/// and only matches a literal `*` level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopicPermission {
    /// Topic pattern, possibly containing `+` or `#` wildcards.
    pub pattern: String,
    /// Whether the user can subscribe to matching topics.
    pub can_subscribe: bool,
    /// Whether the user can publish to matching topics.
    pub can_publish: bool,
}

impl TopicPermission {
    /// Builds a permission from a pattern and both flags.
    ///
    /// Args:
    ///     pattern: The topic pattern to match.
    ///     can_subscribe: Whether matching topics may be subscribed to.
    ///     can_publish: Whether matching topics may be published to.
    ///
    /// Returns:
    ///     The permission.
    pub fn new(pattern: impl Into<String>, can_subscribe: bool, can_publish: bool) -> Self {
        Self {
            pattern: pattern.into(),
            can_subscribe,
            can_publish,
        }
    }

    /// Builds a subscribe-only permission.
    ///
    /// Args:
    ///     pattern: The topic pattern to match.
    ///
    /// Returns:
    ///     A permission with `can_subscribe` set and `can_publish` cleared.
    pub fn read_only(pattern: impl Into<String>) -> Self {
        Self::new(pattern, true, false)
    }

    /// Builds a publish-only permission.
    ///
    /// Args:
    ///     pattern: The topic pattern to match.
    ///
    /// Returns:
    ///     A permission with `can_publish` set and `can_subscribe` cleared.
    pub fn write_only(pattern: impl Into<String>) -> Self {
        Self::new(pattern, false, true)
    }

    /// Builds a permission that allows both subscribe and publish.
    ///
    /// Args:
    ///     pattern: The topic pattern to match.
    ///
    /// Returns:
    ///     A permission with both flags set.
    pub fn full_access(pattern: impl Into<String>) -> Self {
        Self::new(pattern, true, true)
    }

    /// Tests the topic against this permission's pattern, ignoring the flags.
    ///
    /// Args:
    ///     topic: The concrete topic name.
    ///
    /// Returns:
    ///     `true` when `topic_matches_pattern` accepts the topic.
    pub fn matches(&self, topic: &str) -> bool {
        topic_matches_pattern(&self.pattern, topic)
    }
}

/// Tests whether a concrete topic matches a pattern with MQTT-style wildcards.
///
/// Both strings are split on `/` and compared level by level. A `+` level
/// matches any single topic level. A `#` level matches every remaining topic
/// level, and pattern levels after it are ignored. A final `#` also matches
/// zero levels, so `chat/#` matches `chat` itself, and a pattern of just `#`
/// matches every topic. Every other level must be equal, and the pattern and
/// topic must run out at the same time. `*` is compared literally.
///
/// Args:
///     pattern: The pattern, possibly containing `+` or `#`.
///     topic: The concrete topic name.
///
/// Returns:
///     `true` when the topic matches the pattern.
///
/// Example:
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
    if pattern == topic {
        return true;
    }

    if pattern == "#" {
        return true;
    }

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    let mut pattern_idx = 0;
    let mut topic_idx = 0;

    while pattern_idx < pattern_parts.len() && topic_idx < topic_parts.len() {
        let pattern_part = pattern_parts[pattern_idx];

        match pattern_part {
            "#" => return true,
            "+" => {
                pattern_idx += 1;
                topic_idx += 1;
            }
            _ => {
                if pattern_part != topic_parts[topic_idx] {
                    return false;
                }
                pattern_idx += 1;
                topic_idx += 1;
            }
        }
    }

    if pattern_idx == pattern_parts.len() && topic_idx == topic_parts.len() {
        return true;
    }

    if pattern_idx < pattern_parts.len() {
        let remaining = &pattern_parts[pattern_idx..];
        if remaining.len() == 1 && remaining[0] == "#" {
            return true;
        }
    }

    false
}

/// Stateless helpers that decide subscribe and publish access from a permission list.
///
/// Access is granted when any entry both matches the topic and has the
/// relevant flag set. Role handling (admins, disabled users) happens in
/// `AuthService`, not here.
pub struct PermissionChecker;

impl PermissionChecker {
    /// Decides whether a `Role::User` permission list allows subscribing to a topic.
    ///
    /// Args:
    ///     permissions: The user's topic permissions.
    ///     topic: The topic to subscribe to.
    ///
    /// Returns:
    ///     `true` when some permission matches the topic and has `can_subscribe` set.
    pub fn can_subscribe(permissions: &[TopicPermission], topic: &str) -> bool {
        permissions
            .iter()
            .any(|p| p.can_subscribe && p.matches(topic))
    }

    /// Decides whether a `Role::User` permission list allows publishing to a topic.
    ///
    /// Args:
    ///     permissions: The user's topic permissions.
    ///     topic: The topic to publish to.
    ///
    /// Returns:
    ///     `true` when some permission matches the topic and has `can_publish` set.
    pub fn can_publish(permissions: &[TopicPermission], topic: &str) -> bool {
        permissions
            .iter()
            .any(|p| p.can_publish && p.matches(topic))
    }

    /// Decides whether a `Role::ReadOnly` pattern list allows subscribing to a topic.
    ///
    /// Args:
    ///     allowed_topics: The read-only user's topic patterns.
    ///     topic: The topic to subscribe to.
    ///
    /// Returns:
    ///     `true` when any pattern matches the topic.
    pub fn can_subscribe_readonly(allowed_topics: &[String], topic: &str) -> bool {
        allowed_topics
            .iter()
            .any(|pattern| topic_matches_pattern(pattern, topic))
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    /// Checks that a pattern without wildcards matches only the identical topic.
    #[test]
    fn test_exact_match() {
        assert!(topic_matches_pattern("chat/room1", "chat/room1"));
        assert!(!topic_matches_pattern("chat/room1", "chat/room2"));
    }

    /// Checks that a trailing `#` matches the parent level and any depth below it, but not other prefixes.
    #[test]
    fn test_multi_level_wildcard_hash() {
        assert!(topic_matches_pattern("chat/#", "chat"));
        assert!(topic_matches_pattern("chat/#", "chat/room1"));
        assert!(topic_matches_pattern("chat/#", "chat/room1/messages"));
        assert!(!topic_matches_pattern("chat/#", "other/room1"));
    }

    /// Checks that `*` inside a pattern is treated as a literal level, not a wildcard.
    #[test]
    fn test_star_is_not_a_wildcard() {
        assert!(!topic_matches_pattern("chat/*", "chat/room1/messages"));
        assert!(!topic_matches_pattern("chat/*", "other/room1"));
    }

    /// Checks that `+` matches exactly one level and fails when the level count differs.
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

    /// Checks that a lone `#` matches every topic while a lone `*` matches only itself.
    #[test]
    fn test_global_wildcard() {
        assert!(topic_matches_pattern("#", "anything"));
        assert!(topic_matches_pattern("#", "any/thing/at/all"));
        assert!(!topic_matches_pattern("*", "anything"));
        assert!(!topic_matches_pattern("chat/*", "chat/room1"));
        assert!(topic_matches_pattern("chat/*", "chat/*"));
    }

    /// Checks that `PermissionChecker` honours the subscribe and publish flags of matching permissions.
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
