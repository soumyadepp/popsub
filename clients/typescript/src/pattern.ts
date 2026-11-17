/**
 * Topic patterns for wildcard subscriptions: a topic name in which a whole
 * level may be `+` (any one level) and the last level may be `#` (that
 * level and everything below it). Mirrors `popsub_protocol::topics`.
 *
 * @module
 */

/**
 * Whether a string has a wildcard, so it is a pattern and not a plain topic name.
 *
 * @param topic - The string.
 * @returns True when it contains `+` or `#`.
 */
export function isPattern(topic: string): boolean {
  return topic.includes("+") || topic.includes("#");
}

/**
 * Whether a topic matches a pattern. `+` stands for exactly one level and
 * `#`, which ends a pattern, for the rest of the topic, including nothing
 * (`orders/#` matches `orders`).
 *
 * @param pattern - The pattern.
 * @param topic - The topic name.
 * @returns True when the topic matches.
 */
export function matchesPattern(pattern: string, topic: string): boolean {
  if (pattern === topic) {
    return true;
  }
  const levels = pattern.split("/");
  const parts = topic.split("/");
  for (const [index, level] of levels.entries()) {
    if (level === "#") {
      return true;
    }
    const part = parts[index];
    if (part === undefined || (level !== "+" && level !== part)) {
      return false;
    }
  }
  return levels.length === parts.length;
}
