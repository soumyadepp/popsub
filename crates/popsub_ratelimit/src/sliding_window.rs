//! `SlidingWindow`, a per-key limiter that allows at most N requests in any window of time.
//!
//! Each key keeps the timestamps of its requests within the window. There is
//! no burst allowance beyond `max_requests`. The transport uses it, keyed by
//! address, to limit registration attempts.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::RateLimiter;
use crate::keyed::KeyedState;

/// A per-key limiter allowing at most `max_requests` in any `window_size` period.
///
/// Example:
///
/// ```rust
/// use popsub_ratelimit::{RateLimiter, SlidingWindow};
/// use std::time::Duration;
///
/// let limiter = SlidingWindow::new(100, Duration::from_secs(60));
/// assert!(limiter.try_acquire("203.0.113.7"));
/// ```
pub struct SlidingWindow {
    /// The most requests allowed within one window.
    max_requests: u64,
    /// The length of the window.
    window_size: Duration,
    /// Every key's request times, oldest first.
    windows: KeyedState<VecDeque<Instant>>,
}

impl SlidingWindow {
    /// Creates a sliding window limiter.
    ///
    /// Args:
    ///     max_requests: The most requests allowed within one window.
    ///     window_size: The length of the window.
    ///
    /// Returns:
    ///     A limiter with no keys yet.
    pub fn new(max_requests: u64, window_size: Duration) -> Self {
        Self {
            max_requests,
            window_size,
            windows: KeyedState::new(),
        }
    }

    /// Creates a limiter of `max_requests` per second.
    ///
    /// Args:
    ///     max_requests: The most requests allowed per second.
    ///
    /// Returns:
    ///     A limiter with no keys yet.
    pub fn per_second(max_requests: u64) -> Self {
        Self::new(max_requests, Duration::from_secs(1))
    }

    /// Creates a limiter of `max_requests` per minute.
    ///
    /// Args:
    ///     max_requests: The most requests allowed per minute.
    ///
    /// Returns:
    ///     A limiter with no keys yet.
    pub fn per_minute(max_requests: u64) -> Self {
        Self::new(max_requests, Duration::from_secs(60))
    }

    /// Forgets keys with no requests left inside their window.
    ///
    /// A forgotten key behaves exactly like a new one, which is correct
    /// because its window is empty.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     threads add keys at the same time.
    pub fn prune_idle(&self) -> usize {
        let now = Instant::now();
        self.windows.retain(|times| {
            self.drop_expired(times, now);
            !times.is_empty()
        })
    }

    /// Number of keys currently tracked.
    ///
    /// Returns:
    ///     The key count.
    pub fn tracked_keys(&self) -> usize {
        self.windows.len()
    }

    /// The earliest time still inside the window ending at `now`.
    ///
    /// Args:
    ///     now: The end of the window.
    ///
    /// Returns:
    ///     The window's start, or `None` when it reaches back before the
    ///     clock's earliest representable instant (then nothing has expired).
    fn window_start(&self, now: Instant) -> Option<Instant> {
        now.checked_sub(self.window_size)
    }

    /// Removes request times that have left the window.
    ///
    /// Args:
    ///     times: A key's request times, oldest first.
    ///     now: The current time.
    fn drop_expired(&self, times: &mut VecDeque<Instant>, now: Instant) {
        let Some(start) = self.window_start(now) else {
            return;
        };
        while times.front().is_some_and(|&oldest| oldest < start) {
            times.pop_front();
        }
    }

    /// Counts request times still inside the window, without changing them.
    ///
    /// Args:
    ///     times: A key's request times, oldest first.
    ///     now: The current time.
    ///
    /// Returns:
    ///     How many requests count against the limit now.
    fn in_window(&self, times: &VecDeque<Instant>, now: Instant) -> u64 {
        let count = match self.window_start(now) {
            Some(start) => times.iter().filter(|&&t| t >= start).count(),
            None => times.len(),
        };
        u64::try_from(count).unwrap_or(u64::MAX)
    }
}

impl RateLimiter for SlidingWindow {
    /// Records a request for the key if it is under the limit.
    ///
    /// Args:
    ///     key: The key to charge.
    ///
    /// Returns:
    ///     `true` when the request is allowed and recorded, `false` when the
    ///     window is already full.
    fn try_acquire(&self, key: &str) -> bool {
        let now = Instant::now();
        self.windows.update(key, VecDeque::new, |times| {
            self.drop_expired(times, now);
            if (times.len() as u64) < self.max_requests {
                times.push_back(now);
                true
            } else {
                false
            }
        })
    }

    /// Reports how many more requests the key may make now, without creating it.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The remaining allowance. An unknown key has the full allowance.
    fn remaining(&self, key: &str) -> u64 {
        let now = Instant::now();
        self.windows
            .read(key, |times| {
                self.max_requests.saturating_sub(self.in_window(times, now))
            })
            .unwrap_or(self.max_requests)
    }

    /// Forgets the key, so its next request starts a fresh window.
    ///
    /// Args:
    ///     key: The key to reset.
    fn reset(&self, key: &str) {
        self.windows.remove(key);
    }

    /// Reports how long until the key may make another request, without creating it.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `None` when a request is allowed now, otherwise the whole seconds
    ///     (at least 1) until the oldest request in the window expires.
    fn retry_after(&self, key: &str) -> Option<u64> {
        let now = Instant::now();
        self.windows
            .read(key, |times| {
                if self.in_window(times, now) < self.max_requests {
                    return None;
                }
                let start = self.window_start(now)?;
                let oldest = times.iter().find(|&&t| t >= start)?;
                let expires = *oldest + self.window_size;
                Some(expires.saturating_duration_since(now).as_secs().max(1))
            })
            .flatten()
    }
}
