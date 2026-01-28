//! Sliding Window Rate Limiter
//!
//! Provides smooth rate limiting without allowing bursts.
//! Tracks requests in a sliding time window.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::RateLimiter;

/// Request timestamps for a single key.
struct Window {
    /// Timestamps of recent requests.
    timestamps: Mutex<VecDeque<Instant>>,
}

/// Sliding window rate limiter.
///
/// Limits requests to `max_requests` within any `window_size` duration.
/// Unlike token bucket, this doesn't allow bursts.
///
/// # Example
///
/// ```rust
/// use popsub_ratelimit::SlidingWindow;
/// use std::time::Duration;
///
/// // Max 100 requests per minute
/// let limiter = SlidingWindow::new(100, Duration::from_secs(60));
/// ```
pub struct SlidingWindow {
    /// Maximum requests allowed in the window.
    max_requests: u64,
    /// Size of the sliding window.
    window_size: Duration,
    /// Per-key windows.
    windows: DashMap<String, Window>,
}

impl SlidingWindow {
    /// Create a new sliding window rate limiter.
    ///
    /// # Arguments
    ///
    /// * `max_requests` - Maximum requests allowed in the window
    /// * `window_size` - Duration of the sliding window
    pub fn new(max_requests: u64, window_size: Duration) -> Self {
        Self {
            max_requests,
            window_size,
            windows: DashMap::new(),
        }
    }

    /// Create with common presets.
    pub fn per_second(max_requests: u64) -> Self {
        Self::new(max_requests, Duration::from_secs(1))
    }

    pub fn per_minute(max_requests: u64) -> Self {
        Self::new(max_requests, Duration::from_secs(60))
    }

    fn get_or_create_window(&self, key: &str) -> dashmap::mapref::one::Ref<'_, String, Window> {
        // Use entry API to avoid race condition between contains_key and get
        self.windows
            .entry(key.to_string())
            .or_insert_with(|| Window {
                timestamps: Mutex::new(VecDeque::new()),
            });
        // Safe to unwrap: we just inserted if missing
        self.windows.get(key).unwrap()
    }

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

    async fn remaining(&self, key: &str) -> u64 {
        let window = self.get_or_create_window(key);
        let mut timestamps = window.timestamps.lock().unwrap();
        let now = Instant::now();

        self.cleanup_old_entries(&mut timestamps, now);

        self.max_requests.saturating_sub(timestamps.len() as u64)
    }

    async fn reset(&self, key: &str) {
        self.windows.remove(key);
    }

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
    use super::*;

    #[tokio::test]
    async fn test_sliding_window_separate_keys() {
        let limiter = SlidingWindow::new(1, Duration::from_secs(10));

        assert!(limiter.try_acquire("user1").await);
        assert!(limiter.try_acquire("user2").await);

        assert!(!limiter.try_acquire("user1").await);
        assert!(!limiter.try_acquire("user2").await);
    }

    #[tokio::test]
    async fn test_remaining() {
        let limiter = SlidingWindow::new(5, Duration::from_secs(1));

        assert_eq!(limiter.remaining("test").await, 5);
        limiter.try_acquire("test").await;
        assert_eq!(limiter.remaining("test").await, 4);
    }

    #[tokio::test]
    async fn test_window_slides() {
        let limiter = SlidingWindow::new(2, Duration::from_millis(50));

        assert!(limiter.try_acquire("test").await);
        assert!(limiter.try_acquire("test").await);
        assert!(!limiter.try_acquire("test").await);

        // Wait for first request to expire
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Should allow one more
        assert!(limiter.try_acquire("test").await);
    }
}
