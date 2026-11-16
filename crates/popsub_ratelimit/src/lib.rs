//! Per-key rate limiters for PopSub: token bucket, sliding window and login lockout.
//!
//! `TokenBucket` allows bursts up to a capacity and refills at a fixed rate.
//! `SlidingWindow` allows at most a fixed number of requests in any window
//! of a given length, with no burst allowance. Both implement `RateLimiter`.
//! `LoginRateLimiter` counts failed logins per key and locks the key out
//! after too many, with optional exponential backoff on repeated lockouts.
//!
//! All three are synchronous, keep their state in a shared `KeyedState`, and
//! can be shared across threads and tasks behind an `Arc`. Read methods never
//! create state for unknown keys, but every key that is charged stays in
//! memory until it is reset or pruned, so call `prune_idle` periodically.

mod keyed;
mod login_limiter;
mod sliding_window;
mod token_bucket;

#[cfg(test)]
mod tests;

pub use login_limiter::{
    LoginRateLimiter, LoginRateLimiterBuilder, LoginRateLimiterConfig, LoginStatus,
};
pub use sliding_window::SlidingWindow;
pub use token_bucket::TokenBucket;

/// The operations shared by request rate limiters.
pub trait RateLimiter: Send + Sync {
    /// Charges one request to the key if it is within its limit.
    ///
    /// Args:
    ///     key: The key to charge.
    ///
    /// Returns:
    ///     `true` when the request is allowed, `false` when it is limited.
    fn try_acquire(&self, key: &str) -> bool;

    /// Reports how many requests the key could make now, without charging it.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The remaining allowance.
    fn remaining(&self, key: &str) -> u64;

    /// Forgets the key, giving it a fresh allowance.
    ///
    /// Args:
    ///     key: The key to reset.
    fn reset(&self, key: &str);

    /// Reports how long until the key may make a request.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `None` when a request is allowed now, otherwise whole seconds to wait.
    fn retry_after(&self, key: &str) -> Option<u64>;
}
