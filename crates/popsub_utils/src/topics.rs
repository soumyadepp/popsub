//! Topic patterns: matching a topic against one, and checking one is well formed.
//!
//! A pattern is a topic name in which a whole level may be `+` (any one
//! level) and the last level may be `#` (that level and everything below).
//! Permissions, wildcard subscriptions and per-topic retention all use them.

/// The longest pattern accepted, in bytes; the same as the longest topic name.
pub const MAX_PATTERN_BYTES: usize = 120;

/// Whether a topic matches a pattern.
///
/// Example:
///
/// ```
/// use popsub_utils::topics::matches_pattern;
///
/// assert!(matches_pattern("chat/#", "chat/room1/messages"));
/// assert!(matches_pattern("sensors/+/temp", "sensors/attic/temp"));
/// assert!(!matches_pattern("sensors/+/temp", "sensors/attic/humidity"));
/// ```
///
/// Args:
///     pattern: The pattern.
///     topic: The topic name.
///
/// Returns:
///     True when the topic matches.
pub fn matches_pattern(pattern: &str, topic: &str) -> bool {
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

/// Whether a string has a wildcard level, so it is a pattern and not a plain topic name.
///
/// Args:
///     topic: The string.
///
/// Returns:
///     True when it contains `+` or `#`.
pub fn is_pattern(topic: &str) -> bool {
    topic.contains(['+', '#'])
}

/// Checks that a pattern is well formed: 1 to `MAX_PATTERN_BYTES` bytes, no
/// control characters, no empty levels, `+` only as a whole level and `#`
/// only as the whole last level.
///
/// Args:
///     pattern: The pattern.
///
/// Raises:
///     String: Which rule it breaks.
pub fn validate_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("it must be 1 to {MAX_PATTERN_BYTES} bytes"));
    }
    if pattern.chars().any(char::is_control) {
        return Err("it has control characters".to_string());
    }
    let levels: Vec<&str> = pattern.split('/').collect();
    for (index, level) in levels.iter().enumerate() {
        let last = index + 1 == levels.len();
        if level.is_empty() {
            return Err("a level is empty".to_string());
        }
        if level.contains('#') && (*level != "#" || !last) {
            return Err("'#' may only be the whole last level".to_string());
        }
        if level.contains('+') && *level != "+" {
            return Err("'+' must be a whole level".to_string());
        }
    }
    Ok(())
}
