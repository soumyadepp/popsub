//! `TokenBucket`, a per-key limiter that allows bursts and refills at a fixed rate.
//!
//! Each key starts with a full bucket of `capacity` tokens. A request takes
//! one token. Every whole `interval` adds `rate` tokens, up to `capacity`.
//! Time left over from a partial interval carries forward, so the long-run
//! rate matches the configured one. The transport uses it, keyed by
//! username, to limit publishing.

use std::time::{Duration, Instant};

use crate::RateLimiter;
use crate::keyed::KeyedState;

/// One key's bucket.
struct BucketState {
    /// Tokens available now.
    tokens: u64,
    /// The interval boundary refills are counted from.
    last_refill: Instant,
    /// When the key last asked for a token, used to find idle keys.
    last_used: Instant,
}

/// A per-key token bucket: bursts up to `capacity`, refilled by `rate` tokens every `interval`.
///
/// Example:
///
/// ```rust
/// use popsub_ratelimit::{RateLimiter, TokenBucket};
/// use std::time::Duration;
///
/// let limiter = TokenBucket::new(100, 150, Duration::from_secs(1));
/// assert!(limiter.try_acquire("alice"));
/// ```
pub struct TokenBucket {
    /// The most tokens a bucket can hold.
    capacity: u64,
    /// Tokens added per whole interval.
    rate: u64,
    /// How often `rate` tokens are added. Never zero.
    interval: Duration,
    /// Every key's bucket.
    buckets: KeyedState<BucketState>,
}

impl TokenBucket {
    /// Creates a token bucket limiter.
    ///
    /// Panics if `interval` is zero, since the refill rate would be infinite.
    ///
    /// Args:
    ///     rate: Tokens added per interval.
    ///     capacity: The most tokens a bucket holds, which is the burst size.
    ///     interval: How often `rate` tokens are added.
    ///
    /// Returns:
    ///     A limiter with no keys yet.
    pub fn new(rate: u64, capacity: u64, interval: Duration) -> Self {
        assert!(!interval.is_zero(), "TokenBucket interval must not be zero");
        Self {
            capacity,
            rate,
            interval,
            buckets: KeyedState::new(),
        }
    }

    /// Creates a limiter of `rate` tokens per second with a burst of twice that.
    ///
    /// Args:
    ///     rate: Tokens added per second.
    ///
    /// Returns:
    ///     A limiter with no keys yet.
    pub fn per_second(rate: u64) -> Self {
        Self::new(rate, rate.saturating_mul(2), Duration::from_secs(1))
    }

    /// Creates a limiter of `rate` tokens per minute with a burst of twice that.
    ///
    /// Args:
    ///     rate: Tokens added per minute.
    ///
    /// Returns:
    ///     A limiter with no keys yet.
    pub fn per_minute(rate: u64) -> Self {
        Self::new(rate, rate.saturating_mul(2), Duration::from_secs(60))
    }

    /// Forgets buckets that haven't been used for `max_idle`.
    ///
    /// Choose `max_idle` long enough for an empty bucket to refill
    /// completely, so a forgotten key gets no more tokens than it would have
    /// had anyway.
    ///
    /// Args:
    ///     max_idle: How long a bucket must have gone unused before removal.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     threads add keys at the same time.
    pub fn prune_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        self.buckets
            .retain(|bucket| now.duration_since(bucket.last_used) < max_idle)
    }

    /// Number of keys currently tracked.
    ///
    /// Returns:
    ///     The key count.
    pub fn tracked_keys(&self) -> usize {
        self.buckets.len()
    }

    /// Whole intervals that have passed between `since` and `now`.
    ///
    /// Args:
    ///     since: The start of the period.
    ///     now: The end of the period.
    ///
    /// Returns:
    ///     The number of complete intervals, saturating at `u32::MAX`.
    fn whole_intervals(&self, since: Instant, now: Instant) -> u32 {
        let intervals = now.duration_since(since).as_nanos() / self.interval.as_nanos();
        u32::try_from(intervals).unwrap_or(u32::MAX)
    }

    /// Tokens a bucket would hold at `now`, without changing it.
    ///
    /// Args:
    ///     bucket: The bucket to inspect.
    ///     now: The time to evaluate at.
    ///
    /// Returns:
    ///     The token count after any refill due by `now`.
    fn tokens_at(&self, bucket: &BucketState, now: Instant) -> u64 {
        let added =
            u64::from(self.whole_intervals(bucket.last_refill, now)).saturating_mul(self.rate);
        bucket.tokens.saturating_add(added).min(self.capacity)
    }

    /// Applies the refill due by `now`, carrying any partial interval forward.
    ///
    /// Once the bucket is full there is nothing to carry, so the refill
    /// boundary moves to `now`.
    ///
    /// Args:
    ///     bucket: The bucket to refill.
    ///     now: The current time.
    fn refill(&self, bucket: &mut BucketState, now: Instant) {
        let intervals = self.whole_intervals(bucket.last_refill, now);
        if intervals == 0 {
            return;
        }
        bucket.tokens = self.tokens_at(bucket, now);
        bucket.last_refill = if bucket.tokens == self.capacity {
            now
        } else {
            bucket.last_refill + self.interval.saturating_mul(intervals)
        };
    }

    /// Builds the state for a key seen for the first time: a full bucket.
    ///
    /// Args:
    ///     now: The current time.
    ///
    /// Returns:
    ///     A full bucket.
    fn full_bucket(&self, now: Instant) -> BucketState {
        BucketState {
            tokens: self.capacity,
            last_refill: now,
            last_used: now,
        }
    }
}

impl RateLimiter for TokenBucket {
    /// Takes one token from the key's bucket if one is available.
    ///
    /// Args:
    ///     key: The key to charge.
    ///
    /// Returns:
    ///     `true` when a token was taken, `false` when the bucket is empty.
    fn try_acquire(&self, key: &str) -> bool {
        let now = Instant::now();
        self.buckets.update(
            key,
            || self.full_bucket(now),
            |bucket| {
                self.refill(bucket, now);
                bucket.last_used = now;
                if bucket.tokens > 0 {
                    bucket.tokens -= 1;
                    true
                } else {
                    false
                }
            },
        )
    }

    /// Reports how many tokens the key could take now, without creating it.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The available tokens. An unknown key has a full bucket.
    fn remaining(&self, key: &str) -> u64 {
        let now = Instant::now();
        self.buckets
            .read(key, |bucket| self.tokens_at(bucket, now))
            .unwrap_or(self.capacity)
    }

    /// Forgets the key, so its next request starts with a full bucket.
    ///
    /// Args:
    ///     key: The key to reset.
    fn reset(&self, key: &str) {
        self.buckets.remove(key);
    }

    /// Reports how long until the key can take a token, without creating it.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `None` when a token is available now, otherwise the whole seconds
    ///     (at least 1) until the next refill.
    fn retry_after(&self, key: &str) -> Option<u64> {
        let now = Instant::now();
        self.buckets
            .read(key, |bucket| {
                if self.tokens_at(bucket, now) > 0 {
                    return None;
                }
                let elapsed = now.duration_since(bucket.last_refill).as_nanos();
                let into_interval = elapsed % self.interval.as_nanos();
                let wait = self.interval.as_nanos() - into_interval;
                Some(
                    Duration::from_nanos(u64::try_from(wait).unwrap_or(u64::MAX))
                        .as_secs()
                        .max(1),
                )
            })
            .flatten()
    }
}
