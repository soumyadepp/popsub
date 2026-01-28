//! Token Bucket Rate Limiter
//!
//! Classic rate limiting algorithm with burst support.
//! Tokens are added at a fixed rate, and each request consumes one token.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::RateLimiter;

/// Token bucket state for a single key.
/// Both tokens and last_refill are wrapped in a single Mutex to ensure
/// atomic updates and prevent race conditions during refill operations.
struct Bucket {
    /// Current number of tokens and last refill time, protected by a single mutex.
    state: Mutex<BucketState>,
}

/// Internal state of a bucket, protected by a mutex.
struct BucketState {
    /// Current number of tokens available.
    tokens: u64,
    /// Last time tokens were added.
    last_refill: Instant,
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
        // Use entry API to atomically get or insert, avoiding race conditions
        // between contains_key check and get() that could occur with concurrent reset().
        self.buckets
            .entry(key.to_string())
            .or_insert_with(|| Bucket {
                state: Mutex::new(BucketState {
                    tokens: self.capacity,
                    last_refill: Instant::now(),
                }),
            })
            .downgrade()
    }

    /// Refill tokens based on elapsed time and attempt to consume one token.
    /// Returns true if a token was successfully consumed, false otherwise.
    /// This method handles both refilling and consuming atomically under a single lock.
    fn refill_and_try_consume(&self, bucket: &Bucket) -> bool {
        let mut state = bucket.state.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill);

        if elapsed >= self.interval {
            let intervals = elapsed.as_nanos() / self.interval.as_nanos();
            let tokens_to_add = (intervals as u64) * self.rate;
            state.tokens = (state.tokens + tokens_to_add).min(self.capacity);
            state.last_refill = now;
        }

        if state.tokens > 0 {
            state.tokens -= 1;
            true
        } else {
            false
        }
    }

    /// Refill tokens and return the current count.
    fn refill_and_get_remaining(&self, bucket: &Bucket) -> u64 {
        let mut state = bucket.state.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill);

        if elapsed >= self.interval {
            let intervals = elapsed.as_nanos() / self.interval.as_nanos();
            let tokens_to_add = (intervals as u64) * self.rate;
            state.tokens = (state.tokens + tokens_to_add).min(self.capacity);
            state.last_refill = now;
        }

        state.tokens
    }
}

impl RateLimiter for TokenBucket {
    async fn try_acquire(&self, key: &str) -> bool {
        let bucket = self.get_or_create_bucket(key);
        self.refill_and_try_consume(&bucket)
    }

    async fn remaining(&self, key: &str) -> u64 {
        let bucket = self.get_or_create_bucket(key);
        self.refill_and_get_remaining(&bucket)
    }

    async fn reset(&self, key: &str) {
        self.buckets.remove(key);
    }

    async fn retry_after(&self, key: &str) -> Option<u64> {
        let bucket = self.get_or_create_bucket(key);
        let state = bucket.state.lock().unwrap();

        if state.tokens > 0 {
            None
        } else {
            let elapsed = Instant::now().duration_since(state.last_refill);
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
