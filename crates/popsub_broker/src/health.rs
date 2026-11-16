//! `StorageHealth`: whether appends have been failing, for the server's
//! readiness check.
//!
//! Topic writers record every append's outcome. Storage counts as failing
//! when the most recent outcome was a failure and it happened within the
//! window the caller asks about. The window matters: a server marked not
//! ready gets no traffic from its load balancer, so no new append could ever
//! succeed and clear the failure. Once the window passes, the server is
//! ready again, and the next append decides afresh.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The outcomes of recent appends, shared by every topic writer.
///
/// Outcomes are ordered by a counter, not by time, so a failure and a
/// success in the same millisecond still have a clear latest.
#[derive(Debug)]
pub struct StorageHealth {
    /// The clock failure times are measured from.
    started: Instant,
    /// Numbers the outcomes in the order they were recorded.
    outcomes: AtomicU64,
    /// The number of the last failed append; 0 for never.
    last_failure: AtomicU64,
    /// The number of the last successful append; 0 for never.
    last_success: AtomicU64,
    /// Milliseconds after `started` of the last failed append.
    last_failure_ms: AtomicU64,
}

impl Default for StorageHealth {
    /// Builds a record with no outcomes yet.
    ///
    /// Returns:
    ///     The record.
    fn default() -> Self {
        Self {
            started: Instant::now(),
            outcomes: AtomicU64::new(0),
            last_failure: AtomicU64::new(0),
            last_success: AtomicU64::new(0),
            last_failure_ms: AtomicU64::new(0),
        }
    }
}

impl StorageHealth {
    /// Milliseconds since this record was made.
    ///
    /// Returns:
    ///     The time on this record's clock.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Numbers the next outcome.
    ///
    /// Returns:
    ///     A number above every earlier outcome's.
    fn next(&self) -> u64 {
        self.outcomes.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Records a successful append.
    pub(crate) fn record_success(&self) {
        let number = self.next();
        self.last_success.fetch_max(number, Ordering::SeqCst);
    }

    /// Records a failed append.
    pub(crate) fn record_failure(&self) {
        self.last_failure_ms.store(self.now_ms(), Ordering::SeqCst);
        let number = self.next();
        self.last_failure.fetch_max(number, Ordering::SeqCst);
    }

    /// Whether appends are failing: the latest outcome was a failure, within `window`.
    ///
    /// Args:
    ///     window: How long a failure counts for.
    ///
    /// Returns:
    ///     How long ago the failure was, or `None` when storage isn't failing.
    pub fn failing(&self, window: Duration) -> Option<Duration> {
        let failure = self.last_failure.load(Ordering::SeqCst);
        if failure == 0 || failure < self.last_success.load(Ordering::SeqCst) {
            return None;
        }
        let at = self.last_failure_ms.load(Ordering::SeqCst);
        let ago = Duration::from_millis(self.now_ms().saturating_sub(at));
        (ago < window).then_some(ago)
    }
}

/// Unit tests for the failure window.
#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that a failure counts until a success or until the window passes.
    #[test]
    fn failures_count_until_a_success_or_the_window_passes() {
        let health = StorageHealth::default();
        assert_eq!(health.failing(Duration::from_secs(30)), None);
        health.record_failure();
        assert!(health.failing(Duration::from_secs(30)).is_some());
        assert_eq!(health.failing(Duration::ZERO), None);
        health.record_success();
        assert_eq!(health.failing(Duration::from_secs(30)), None);
        health.record_failure();
        assert!(health.failing(Duration::from_secs(30)).is_some());
    }
}
