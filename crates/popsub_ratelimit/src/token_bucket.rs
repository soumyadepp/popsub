//! `TokenBucket`, a rate limiter that allows bursts up to a capacity and refills at a fixed rate.
//!
//! Each key has a bucket that starts full. A request takes one token, and
//! `rate` tokens are added for every whole `interval` that has passed, up
//! to `capacity`. The transport uses it to limit publishing.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::RateLimiter;

/// Token bucket for a single key.
///
/// The token count and the last refill time sit behind one mutex so a
/// refill and the consume that follows it happen as one step.
struct Bucket {
    /// Token count and last refill time.
    state: Mutex<BucketState>,
}

/// Token count and refill time of one bucket.
struct BucketState {
    /// Tokens currently available.
    tokens: u64,
    /// When tokens were last added, or when the bucket was created.
    last_refill: Instant,
}

/// Rate limiter that allows bursts up to `capacity` and refills `rate` tokens per `interval`.
///
/// Example, 100 requests per second with bursts of up to 150:
///
/// ```rust
/// use popsub_ratelimit::TokenBucket;
/// use std::time::Duration;
///
/// let limiter = TokenBucket::new(100, 150, Duration::from_secs(1));
/// ```
pub struct TokenBucket {
    /// Maximum tokens a bucket holds, which is the largest burst allowed.
    capacity: u64,
    /// Tokens added per interval.
    rate: u64,
    /// How often `rate` tokens are added.
    interval: Duration,
    /// Buckets by key.
    buckets: DashMap<String, Bucket>,
}

impl TokenBucket {
    /// Creates a token bucket rate limiter.
    ///
    /// Args:
    ///     rate: Tokens added to each bucket per interval.
    ///     capacity: Maximum tokens per bucket, and the number a new bucket starts with.
    ///     interval: How often `rate` tokens are added. Must not be zero, or
    ///         the first refill divides by zero and panics.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn new(rate: u64, capacity: u64, interval: Duration) -> Self {
        Self {
            capacity,
            rate,
            interval,
            buckets: DashMap::new(),
        }
    }

    /// Creates a limiter that adds `rate` tokens every second, with a capacity of twice `rate`.
    ///
    /// Args:
    ///     rate: Tokens added per key per second.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn per_second(rate: u64) -> Self {
        Self::new(rate, rate * 2, Duration::from_secs(1))
    }

    /// Creates a limiter that adds `rate` tokens every minute, with a capacity of twice `rate`.
    ///
    /// Args:
    ///     rate: Tokens added per key per minute.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn per_minute(rate: u64) -> Self {
        Self::new(rate, rate * 2, Duration::from_secs(60))
    }

    /// Returns the bucket for `key`, inserting a full one if the key is new.
    ///
    /// Uses the map's entry API so a concurrent `reset` can't remove the key
    /// between the lookup and the insert, which would otherwise panic.
    ///
    /// Args:
    ///     key: The key to look up.
    ///
    /// Returns:
    ///     A read guard on the key's map entry.
    fn get_or_create_bucket(&self, key: &str) -> dashmap::mapref::one::Ref<'_, String, Bucket> {
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

    /// Forgets buckets that have not been refilled for `max_idle`.
    ///
    /// Call this periodically so buckets for clients that have gone away
    /// don't accumulate. Choose `max_idle` long enough for an empty bucket to
    /// have refilled completely, so a pruned key gets no more tokens than it
    /// would have had anyway. Idle time is measured from the last refill,
    /// which only moves when the bucket is used at least one interval after
    /// the previous refill, so `max_idle` should also be longer than
    /// `interval`. Panics if a bucket's mutex is poisoned.
    ///
    /// Args:
    ///     max_idle: How long a bucket must have been unused before removal.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     tasks add keys at the same time.
    pub fn prune_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        let before = self.buckets.len();
        self.buckets.retain(|_, bucket| {
            let state = bucket.state.get_mut().unwrap();
            now.duration_since(state.last_refill) < max_idle
        });
        before.saturating_sub(self.buckets.len())
    }

    /// Reports how many keys the limiter currently holds state for.
    ///
    /// Returns:
    ///     The number of tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.buckets.len()
    }

    /// Refills a bucket for the time that has passed and takes one token if any are left.
    ///
    /// Both steps run under the bucket's mutex. A refill adds `rate` tokens
    /// per whole interval elapsed and then sets `last_refill` to now, so time
    /// left over from a partial interval is discarded. Panics if the mutex
    /// is poisoned.
    ///
    /// Args:
    ///     bucket: The bucket to refill and take from.
    ///
    /// Returns:
    ///     `true` if a token was taken, `false` if the bucket was empty.
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

    /// Refills a bucket for the time that has passed and reports its token count.
    ///
    /// Refills the same way as `refill_and_try_consume`. Panics if the mutex
    /// is poisoned.
    ///
    /// Args:
    ///     bucket: The bucket to refill.
    ///
    /// Returns:
    ///     The tokens available after the refill.
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
    /// Takes one token from the bucket for `key`, creating a full bucket if the key is new.
    ///
    /// Args:
    ///     key: The client or caller the request is counted against.
    ///
    /// Returns:
    ///     `true` if a token was taken, `false` if the bucket was empty.
    async fn try_acquire(&self, key: &str) -> bool {
        let bucket = self.get_or_create_bucket(key);
        self.refill_and_try_consume(&bucket)
    }

    /// Reports the tokens left in the bucket for `key` after refilling it.
    ///
    /// Starts tracking `key` with a full bucket if it was not tracked yet.
    ///
    /// Args:
    ///     key: The client or caller to check.
    ///
    /// Returns:
    ///     The number of tokens available.
    async fn remaining(&self, key: &str) -> u64 {
        let bucket = self.get_or_create_bucket(key);
        self.refill_and_get_remaining(&bucket)
    }

    /// Removes the bucket for `key`, so its next request gets a full bucket.
    ///
    /// Args:
    ///     key: The client or caller to reset.
    async fn reset(&self, key: &str) {
        self.buckets.remove(key);
    }

    /// Reports how long until the bucket for `key` next refills, if it is empty.
    ///
    /// Does not refill first, so a bucket that is empty but due a refill
    /// reports 1 second although the next request would succeed. Starts
    /// tracking `key` with a full bucket if it was not tracked yet. Panics if
    /// the bucket's mutex is poisoned.
    ///
    /// Args:
    ///     key: The client or caller to check.
    ///
    /// Returns:
    ///     `None` if the bucket has tokens, otherwise the seconds until the
    ///     next refill, rounded down and at least 1.
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
    //! Unit tests for `TokenBucket`.

    use super::*;

    /// Checks that each key has its own bucket.
    #[tokio::test]
    async fn test_token_bucket_separate_keys() {
        let limiter = TokenBucket::new(1, 1, Duration::from_secs(10));

        assert!(limiter.try_acquire("user1").await);
        assert!(limiter.try_acquire("user2").await);

        assert!(!limiter.try_acquire("user1").await);
        assert!(!limiter.try_acquire("user2").await);
    }

    /// Checks that `remaining` starts at capacity and drops by one per request.
    #[tokio::test]
    async fn test_remaining() {
        let limiter = TokenBucket::new(5, 5, Duration::from_secs(1));

        assert_eq!(limiter.remaining("test").await, 5);
        limiter.try_acquire("test").await;
        assert_eq!(limiter.remaining("test").await, 4);
    }

    /// Checks that `reset` gives an exhausted key a full bucket again.
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
