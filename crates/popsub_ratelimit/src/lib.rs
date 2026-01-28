//! # popsub_ratelimit
//!
//! A flexible, reusable rate limiting library for PopSub.
//!
//! ## Features
//!
//! - **Token Bucket**: Classic rate limiting with burst support
//! - **Sliding Window**: Smooth rate limiting without burst
//! - **Login Rate Limiter**: Specialized for authentication with lockout support
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use popsub_ratelimit::{RateLimiter, TokenBucket};
//! use std::time::Duration;
//!
//! // Create a rate limiter: 10 requests per second, burst of 20
//! let limiter = TokenBucket::new(10, 20, Duration::from_secs(1));
//!
//! // Check if request is allowed
//! if limiter.try_acquire("user_123").await {
//!     // Process request
//! } else {
//!     // Rate limited
//! }
//! ```
//!
//! ## Login Rate Limiting
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
//! // On login attempt
//! if !limiter.check_allowed("username").await {
//!     // Return "too many attempts" error
//! }
//!
//! // On failed login
//! limiter.record_failure("username").await;
//!
//! // On successful login
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

/// Common trait for all rate limiters.
/// The async_fn_in_trait lint is allowed here because this trait is used
/// internally and we don't need to expose Send bounds on the futures.
#[allow(async_fn_in_trait)]
pub trait RateLimiter: Send + Sync {
    /// Try to acquire permission for a request.
    /// Returns `true` if allowed, `false` if rate limited.
    async fn try_acquire(&self, key: &str) -> bool;

    /// Get the number of remaining requests for a key.
    async fn remaining(&self, key: &str) -> u64;

    /// Reset the rate limit for a key.
    async fn reset(&self, key: &str);

    /// Get time until the rate limit resets (in seconds).
    async fn retry_after(&self, key: &str) -> Option<u64>;
}
