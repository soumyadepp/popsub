//! `SlidingWindow`, a rate limiter that allows a fixed number of requests in any window of a given length.
//!
//! Each key keeps the timestamps of its requests inside the current window,
//! so memory per key grows with `max_requests`. Unlike `TokenBucket`, a
//! key that has been quiet gains no extra allowance for a burst.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::RateLimiter;

/// Request timestamps for a single key.
struct Window {
    /// Times of the key's requests, oldest first. Entries older than the window are removed lazily.
    timestamps: Mutex<VecDeque<Instant>>,
}

/// Rate limiter that allows at most `max_requests` per key in any `window_size` period.
///
/// Example, at most 100 requests per minute:
///
/// ```rust
/// use popsub_ratelimit::SlidingWindow;
/// use std::time::Duration;
///
/// let limiter = SlidingWindow::new(100, Duration::from_secs(60));
/// ```
pub struct SlidingWindow {
    /// Maximum requests allowed in the window.
    max_requests: u64,
    /// Length of the sliding window.
    window_size: Duration,
    /// Request timestamps by key.
    windows: DashMap<String, Window>,
}

impl SlidingWindow {
    /// Creates a sliding window rate limiter.
    ///
    /// Args:
    ///     max_requests: Maximum requests allowed per key within the window.
    ///     window_size: Length of the sliding window.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn new(max_requests: u64, window_size: Duration) -> Self {
        Self {
            max_requests,
            window_size,
            windows: DashMap::new(),
        }
    }

    /// Creates a limiter with a one-second window.
    ///
    /// Args:
    ///     max_requests: Maximum requests allowed per key per second.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn per_second(max_requests: u64) -> Self {
        Self::new(max_requests, Duration::from_secs(1))
    }

    /// Creates a limiter with a one-minute window.
    ///
    /// Args:
    ///     max_requests: Maximum requests allowed per key per minute.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn per_minute(max_requests: u64) -> Self {
        Self::new(max_requests, Duration::from_secs(60))
    }

    /// Returns the window for `key`, inserting an empty one if the key is new.
    ///
    /// Uses the map's entry API so a concurrent `reset` can't remove the key
    /// between the lookup and the insert.
    ///
    /// Args:
    ///     key: The key to look up.
    ///
    /// Returns:
    ///     A read guard on the key's map entry.
    fn get_or_create_window(&self, key: &str) -> dashmap::mapref::one::Ref<'_, String, Window> {
        self.windows
            .entry(key.to_string())
            .or_insert_with(|| Window {
                timestamps: Mutex::new(VecDeque::new()),
            })
            .downgrade()
    }

    /// Forgets keys whose window holds no requests any more.
    ///
    /// Call this periodically so keys from clients that have gone away don't
    /// accumulate. A pruned key behaves exactly like a key seen for the first
    /// time, which is correct because its window is empty. Panics if a key's
    /// mutex is poisoned.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     tasks add keys at the same time.
    pub fn prune_idle(&self) -> usize {
        let now = Instant::now();
        let before = self.windows.len();
        self.windows.retain(|_, window| {
            let timestamps = window.timestamps.get_mut().unwrap();
            self.cleanup_old_entries(timestamps, now);
            !timestamps.is_empty()
        });
        before.saturating_sub(self.windows.len())
    }

    /// Reports how many keys the limiter currently holds state for.
    ///
    /// Returns:
    ///     The number of tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.windows.len()
    }

    /// Removes timestamps that have fallen out of the window ending at `now`.
    ///
    /// Panics if `window_size` reaches back before the earliest `Instant`
    /// the platform can represent.
    ///
    /// Args:
    ///     timestamps: The key's request times, oldest first.
    ///     now: The end of the window.
    fn cleanup_old_entries(&self, timestamps: &mut VecDeque<Instant>, now: Instant) {
        let cutoff = now - self.window_size;
        while let Some(&front) = timestamps.front() {
            if front < cutoff {
                timestamps.pop_front();
            } else {
                break;
            }
        }
    }
}

impl RateLimiter for SlidingWindow {
    /// Records a request for `key` if fewer than `max_requests` fall in the current window.
    ///
    /// Args:
    ///     key: The client or caller the request is counted against.
    ///
    /// Returns:
    ///     `true` if the request was recorded, `false` if the window is full.
    async fn try_acquire(&self, key: &str) -> bool {
        let window = self.get_or_create_window(key);
        let mut timestamps = window.timestamps.lock().unwrap();
        let now = Instant::now();

        self.cleanup_old_entries(&mut timestamps, now);

        if timestamps.len() < self.max_requests as usize {
            timestamps.push_back(now);
            true
        } else {
            false
        }
    }

    /// Reports how many requests `key` can still make in the current window.
    ///
    /// Starts tracking `key` if it was not tracked yet.
    ///
    /// Args:
    ///     key: The client or caller to check.
    ///
    /// Returns:
    ///     `max_requests` minus the requests in the current window.
    async fn remaining(&self, key: &str) -> u64 {
        let window = self.get_or_create_window(key);
        let mut timestamps = window.timestamps.lock().unwrap();
        let now = Instant::now();

        self.cleanup_old_entries(&mut timestamps, now);

        self.max_requests.saturating_sub(timestamps.len() as u64)
    }

    /// Removes the window for `key`.
    ///
    /// Args:
    ///     key: The client or caller to reset.
    async fn reset(&self, key: &str) {
        self.windows.remove(key);
    }

    /// Reports how long until the oldest request in a full window expires.
    ///
    /// Starts tracking `key` if it was not tracked yet.
    ///
    /// Args:
    ///     key: The client or caller to check.
    ///
    /// Returns:
    ///     `None` if the window has room, otherwise the seconds until the
    ///     oldest request leaves it, rounded down and at least 1.
    async fn retry_after(&self, key: &str) -> Option<u64> {
        let window = self.get_or_create_window(key);
        let mut timestamps = window.timestamps.lock().unwrap();
        let now = Instant::now();

        self.cleanup_old_entries(&mut timestamps, now);

        if timestamps.len() < self.max_requests as usize {
            None
        } else if let Some(&oldest) = timestamps.front() {
            let expires = oldest + self.window_size;
            if expires > now {
                Some((expires - now).as_secs().max(1))
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `SlidingWindow`.

    use super::*;

    /// Checks that each key has its own window.
    #[tokio::test]
    async fn test_sliding_window_separate_keys() {
        let limiter = SlidingWindow::new(1, Duration::from_secs(10));

        assert!(limiter.try_acquire("user1").await);
        assert!(limiter.try_acquire("user2").await);

        assert!(!limiter.try_acquire("user1").await);
        assert!(!limiter.try_acquire("user2").await);
    }

    /// Checks that `remaining` starts at `max_requests` and drops by one per request.
    #[tokio::test]
    async fn test_remaining() {
        let limiter = SlidingWindow::new(5, Duration::from_secs(1));

        assert_eq!(limiter.remaining("test").await, 5);
        limiter.try_acquire("test").await;
        assert_eq!(limiter.remaining("test").await, 4);
    }

    /// Checks that a full window accepts a request again once its oldest entry has expired.
    #[tokio::test]
    async fn test_window_slides() {
        let limiter = SlidingWindow::new(2, Duration::from_millis(50));

        assert!(limiter.try_acquire("test").await);
        assert!(limiter.try_acquire("test").await);
        assert!(!limiter.try_acquire("test").await);

        tokio::time::sleep(Duration::from_millis(60)).await;

        assert!(limiter.try_acquire("test").await);
    }
}
