//! Token Bucket Rate Limiter
//!
//! Classic rate limiting algorithm with burst support.
//! Tokens are added at a fixed rate, and each request consumes one token.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::RateLimiter;

/// Token bucket state for a single key.
struct Bucket {
    /// Current number of tokens available.
    tokens: AtomicU64,
    /// Last time tokens were added.
    last_refill: std::sync::Mutex<Instant>,
}

/// Token Bucket rate limiter.
///
/// Allows bursts up to `capacity` and refills at `rate` tokens per `interval`.
///
/// # Example
///
/// ```rust
/// use popsub_ratelimit::TokenBucket;
/// use std::time::Duration;
///
/// // 100 requests per second, burst of 150
/// let limiter = TokenBucket::new(100, 150, Duration::from_secs(1));
/// ```
pub struct TokenBucket {
    /// Maximum tokens (burst capacity).
    capacity: u64,
    /// Tokens added per interval.
    rate: u64,
    /// Time interval for refilling tokens.
    interval: Duration,
    /// Per-key buckets.
    buckets: DashMap<String, Bucket>,
}

impl TokenBucket {
    /// Create a new token bucket rate limiter.
    ///
    /// # Arguments
    ///
    /// * `rate` - Number of tokens to add per interval
    /// * `capacity` - Maximum tokens (burst size)
    /// * `interval` - Time interval for token refill
    pub fn new(rate: u64, capacity: u64, interval: Duration) -> Self {
        Self {
            capacity,
            rate,
            interval,
            buckets: DashMap::new(),
        }
    }

    /// Create with common presets.
    pub fn per_second(rate: u64) -> Self {
        Self::new(rate, rate * 2, Duration::from_secs(1))
    }

    pub fn per_minute(rate: u64) -> Self {
        Self::new(rate, rate * 2, Duration::from_secs(60))
    }

    fn get_or_create_bucket(&self, key: &str) -> dashmap::mapref::one::Ref<'_, String, Bucket> {
        if !self.buckets.contains_key(key) {
            self.buckets.insert(
                key.to_string(),
                Bucket {
                    tokens: AtomicU64::new(self.capacity),
                    last_refill: std::sync::Mutex::new(Instant::now()),
                },
            );
        }
        self.buckets.get(key).unwrap()
    }

    fn refill_tokens(&self, bucket: &Bucket) {
        let mut last_refill = bucket.last_refill.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(*last_refill);

        if elapsed >= self.interval {
            let intervals = elapsed.as_nanos() / self.interval.as_nanos();
            let tokens_to_add = (intervals as u64) * self.rate;

            let current = bucket.tokens.load(Ordering::Relaxed);
            let new_tokens = (current + tokens_to_add).min(self.capacity);
            bucket.tokens.store(new_tokens, Ordering::Relaxed);

            *last_refill = now;
        }
    }
}

impl RateLimiter for TokenBucket {
    async fn try_acquire(&self, key: &str) -> bool {
        let bucket = self.get_or_create_bucket(key);
        self.refill_tokens(&bucket);

        loop {
            let current = bucket.tokens.load(Ordering::Relaxed);
            if current == 0 {
                return false;
            }

            if bucket
                .tokens
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    async fn remaining(&self, key: &str) -> u64 {
        let bucket = self.get_or_create_bucket(key);
        self.refill_tokens(&bucket);
        bucket.tokens.load(Ordering::Relaxed)
    }

    async fn reset(&self, key: &str) {
        self.buckets.remove(key);
    }

    async fn retry_after(&self, key: &str) -> Option<u64> {
        let bucket = self.get_or_create_bucket(key);
        let current = bucket.tokens.load(Ordering::Relaxed);

        if current > 0 {
            None
        } else {
            let last_refill = bucket.last_refill.lock().unwrap();
            let elapsed = Instant::now().duration_since(*last_refill);
            let remaining = self.interval.saturating_sub(elapsed);
            Some(remaining.as_secs().max(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_bucket_separate_keys() {
        let limiter = TokenBucket::new(1, 1, Duration::from_secs(10));

        assert!(limiter.try_acquire("user1").await);
        assert!(limiter.try_acquire("user2").await);

        // Each key has its own bucket
        assert!(!limiter.try_acquire("user1").await);
        assert!(!limiter.try_acquire("user2").await);
    }

    #[tokio::test]
    async fn test_remaining() {
        let limiter = TokenBucket::new(5, 5, Duration::from_secs(1));

        assert_eq!(limiter.remaining("test").await, 5);
        limiter.try_acquire("test").await;
        assert_eq!(limiter.remaining("test").await, 4);
    }

    #[tokio::test]
    async fn test_reset() {
        let limiter = TokenBucket::new(2, 2, Duration::from_secs(10));

        limiter.try_acquire("test").await;
        limiter.try_acquire("test").await;
        assert!(!limiter.try_acquire("test").await);

        limiter.reset("test").await;
        assert!(limiter.try_acquire("test").await);
    }
}
