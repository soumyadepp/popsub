//! Works out where a subscriber's replay starts and whether it was cut short.
//!
//! Kept separate from the topic writer so the rules can be tested without
//! storage or channels.

use popsub_storage::LogBounds;

use crate::frame::StartFrom;

/// The replay a subscribe asks for, resolved against the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayPlan {
    /// The first `seq` to send. At or past `bounds.next_seq()` means no replay.
    pub start: u64,
    /// Whether records the subscriber asked for were already trimmed, so it
    /// must be sent a `truncated` frame first.
    pub truncated: bool,
}

/// Resolves a `from` option against a topic's bounds.
///
/// `last` and `seq` count as truncated when the requested start is older
/// than the oldest retained record and something has been trimmed.
/// `since_ms` can't know the publish times of trimmed records, so it counts
/// as truncated when something was trimmed and either nothing is retained or
/// the oldest retained record is the first match. That can report a
/// truncation when every trimmed record was in fact older than the requested
/// time; it never misses a real one.
///
/// Args:
///     from: What the subscriber asked for.
///     bounds: The topic's bounds when the subscribe is handled.
///     seq_at: For `since_ms`, the log's answer to `seq_at` for that time.
///         Ignored for the other options.
///
/// Returns:
///     Where replay starts and whether to report a truncation.
pub(crate) fn plan(from: StartFrom, bounds: LogBounds, seq_at: Option<u64>) -> ReplayPlan {
    let trimmed = bounds.first_seq > 1;
    let from_requested = |requested: u64| ReplayPlan {
        start: requested.max(bounds.first_seq),
        truncated: trimmed && requested < bounds.first_seq,
    };
    match from {
        StartFrom::New => ReplayPlan {
            start: bounds.next_seq(),
            truncated: false,
        },
        StartFrom::Last(count) => from_requested(bounds.next_seq().saturating_sub(count).max(1)),
        StartFrom::Seq(seq) => from_requested(seq.max(1)),
        StartFrom::SinceMs(_) => ReplayPlan {
            start: seq_at.unwrap_or(bounds.next_seq()),
            truncated: trimmed && (bounds.is_empty() || seq_at == Some(bounds.first_seq)),
        },
    }
}
