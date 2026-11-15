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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reset_never_panics() {
    let token = std::sync::Arc::new(TokenBucket::new(1000, 1000, Duration::from_secs(1)));
    let window = std::sync::Arc::new(SlidingWindow::new(1000, Duration::from_secs(1)));
    let login = std::sync::Arc::new(LoginRateLimiter::new());

    let mut tasks = Vec::new();
    for worker in 0..8 {
        let token = token.clone();
        let window = window.clone();
        let login = login.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..2000 {
                if worker % 2 == 0 {
                    token.try_acquire("shared").await;
                    window.try_acquire("shared").await;
                    login.check_allowed("shared").await;
                } else {
                    token.reset("shared").await;
                    window.reset("shared").await;
                    login.prune_idle(Duration::ZERO);
                }
            }
        }));
    }
    for task in tasks {
        task.await
            .expect("a limiter panicked under concurrent reset");
    }
}

#[tokio::test]
async fn token_bucket_prunes_only_idle_keys() {
    let limiter = TokenBucket::new(1, 1, Duration::from_millis(10));
    limiter.try_acquire("old").await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    limiter.try_acquire("fresh").await;

    assert_eq!(limiter.prune_idle(Duration::from_millis(40)), 1);
    assert_eq!(limiter.tracked_keys(), 1);
    assert_eq!(limiter.remaining("fresh").await, 0);
}

#[tokio::test]
async fn sliding_window_prunes_empty_windows() {
    let limiter = SlidingWindow::new(5, Duration::from_millis(30));
    limiter.try_acquire("old").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    limiter.try_acquire("fresh").await;

    assert_eq!(limiter.prune_idle(), 1);
    assert_eq!(limiter.tracked_keys(), 1);
}

#[tokio::test]
async fn login_limiter_keeps_locked_and_recent_keys() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(1)
        .lockout_duration(Duration::from_secs(60))
        .build();
    limiter.record_failure("locked").await;
    limiter.check_allowed("never-failed").await;
    limiter.record_success("succeeded").await;

    assert_eq!(limiter.prune_idle(Duration::from_secs(60)), 2);
    assert!(limiter.is_locked("locked").await);
}
