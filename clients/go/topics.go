package popsub

import "strings"

// isPattern reports whether a subscribe target has a wildcard level, so it
// is a pattern and not a plain topic name.
func isPattern(topic string) bool {
	return strings.ContainsAny(topic, "+#")
}

// matchesPattern reports whether a topic matches a pattern, level by
// level: + matches any one whole level and a trailing # matches the rest,
// including nothing (orders/# matches orders). A pattern without
// wildcards matches only itself.
func matchesPattern(pattern, topic string) bool {
	if pattern == topic || pattern == "#" {
		return true
	}
	patternLevels := strings.Split(pattern, "/")
	topicLevels := strings.Split(topic, "/")
	index := 0
	for index < len(patternLevels) && index < len(topicLevels) {
		switch level := patternLevels[index]; {
		case level == "#":
			return true
		case level != "+" && level != topicLevels[index]:
			return false
		}
		index++
	}
	if index == len(patternLevels) {
		return index == len(topicLevels)
	}
	return len(patternLevels)-index == 1 && patternLevels[index] == "#"
}
