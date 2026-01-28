//! Integration tests for rate limiters.

use super::*;
use std::time::Duration;

#[tokio::test]
async fn test_token_bucket_basic() {
    let limiter = TokenBucket::new(2, 2, Duration::from_millis(100));

    // Should allow first 2 requests (burst)
    assert!(limiter.try_acquire("test").await);
    assert!(limiter.try_acquire("test").await);

    // Should deny 3rd request
    assert!(!limiter.try_acquire("test").await);

    // Wait for refill
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Should allow again
    assert!(limiter.try_acquire("test").await);
}

#[tokio::test]
async fn test_sliding_window_basic() {
    let limiter = SlidingWindow::new(2, Duration::from_millis(100));

    assert!(limiter.try_acquire("test").await);
    assert!(limiter.try_acquire("test").await);
    assert!(!limiter.try_acquire("test").await);

    // Wait for window to slide
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(limiter.try_acquire("test").await);
}

#[tokio::test]
async fn test_login_limiter() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(3)
        .lockout_duration(Duration::from_millis(100))
        .build();

    // First 3 failures allowed
    assert!(limiter.check_allowed("user").await);
    limiter.record_failure("user").await;

    assert!(limiter.check_allowed("user").await);
    limiter.record_failure("user").await;

    assert!(limiter.check_allowed("user").await);
    limiter.record_failure("user").await;

    // 4th attempt blocked
    assert!(!limiter.check_allowed("user").await);

    // Wait for lockout to expire
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Should be allowed again
    assert!(limiter.check_allowed("user").await);
}

#[tokio::test]
async fn test_login_success_resets() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(3)
        .lockout_duration(Duration::from_secs(300))
        .build();

    limiter.record_failure("user").await;
    limiter.record_failure("user").await;

    // Success should reset counter
    limiter.record_success("user").await;

    // Should have full attempts again
    limiter.record_failure("user").await;
    limiter.record_failure("user").await;
    limiter.record_failure("user").await;

    // Now locked out
    assert!(!limiter.check_allowed("user").await);
}
