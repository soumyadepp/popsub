//! Per-key rate limiters for PopSub: token bucket, sliding window and login lockout.
//!
//! `TokenBucket` allows bursts up to a capacity and refills at a fixed rate.
//! `SlidingWindow` allows at most a fixed number of requests in any window
//! of a given length, with no burst allowance. Both implement `RateLimiter`.
//! `LoginRateLimiter` counts failed logins per key and locks the key out
//! after too many, with optional exponential backoff on repeated lockouts.
//! All three keep state per key in a `DashMap` and can be shared across
//! tasks behind an `Arc`. A key stays in memory until it is reset or
//! removed by `prune_idle`, so call `prune_idle` periodically.
//!
//! Example:
//!
//! ```rust,ignore
//! use popsub_ratelimit::{RateLimiter, TokenBucket};
//! use std::time::Duration;
//!
//! let limiter = TokenBucket::new(10, 20, Duration::from_secs(1));
//! let allowed = limiter.try_acquire("user_123").await;
//! ```
//!
//! Login limiting: call `check_allowed` before an attempt, `record_failure`
//! after a failed one and `record_success` after a successful one.
//!
//! ```rust,ignore
//! use popsub_ratelimit::LoginRateLimiter;
//! use std::time::Duration;
//!
//! let limiter = LoginRateLimiter::builder()
//!     .max_attempts(5)
//!     .lockout_duration(Duration::from_secs(300))
//!     .build();
//!
//! let allowed = limiter.check_allowed("username").await;
//! limiter.record_failure("username").await;
//! limiter.record_success("username").await;
//! ```

mod login_limiter;
mod sliding_window;
mod token_bucket;

#[cfg(test)]
mod tests;

pub use login_limiter::{LoginRateLimiter, LoginRateLimiterBuilder, LoginStatus};
pub use sliding_window::SlidingWindow;
pub use token_bucket::TokenBucket;

/// A rate limiter that tracks requests separately for each key.
///
/// The methods are `async fn` so callers can await them uniformly, but the
/// current implementations never yield. `async_fn_in_trait` is allowed
/// because the returned futures carry no `Send` bound, so the trait can't be
/// used where a `Send` future is required from a generic `T: RateLimiter`.
#[allow(async_fn_in_trait)]
pub trait RateLimiter: Send + Sync {
    /// Records a request for `key` if the limit allows it.
    ///
    /// Args:
    ///     key: The client or caller the request is counted against.
    ///
    /// Returns:
    ///     `true` if the request is allowed and was counted, `false` if it is rate limited.
    async fn try_acquire(&self, key: &str) -> bool;

    /// Reports how many more requests `key` can make right now.
    ///
    /// Args:
    ///     key: The client or caller to check.
    ///
    /// Returns:
    ///     The number of requests still allowed.
    async fn remaining(&self, key: &str) -> u64;

    /// Forgets all state for `key`, so its next request is treated as its first.
    ///
    /// Args:
    ///     key: The client or caller to reset.
    async fn reset(&self, key: &str);

    /// Reports how long `key` should wait before its next request can succeed.
    ///
    /// Args:
    ///     key: The client or caller to check.
    ///
    /// Returns:
    ///     `None` if a request would be allowed now, otherwise the wait in
    ///     whole seconds, at least 1.
    async fn retry_after(&self, key: &str) -> Option<u64>;
}
