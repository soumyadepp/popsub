//! Tests for the token bucket, sliding window and login limiters.
//!
//! Most tests use short real durations, so they sleep for tens of
//! milliseconds at most.

use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use super::*;

/// Checks that a bucket allows its burst, refuses the next request, and refills after an interval.
#[test]
fn token_bucket_allows_burst_then_refills() {
    let limiter = TokenBucket::new(2, 2, Duration::from_millis(100));
    assert!(limiter.try_acquire("test"));
    assert!(limiter.try_acquire("test"));
    assert!(!limiter.try_acquire("test"));

    sleep(Duration::from_millis(150));

    assert!(limiter.try_acquire("test"));
}

/// Checks that each key has its own bucket.
#[test]
fn token_bucket_keys_are_independent() {
    let limiter = TokenBucket::new(1, 1, Duration::from_secs(60));
    assert!(limiter.try_acquire("user1"));
    assert!(!limiter.try_acquire("user1"));
    assert!(limiter.try_acquire("user2"));
}

/// Checks that `remaining` counts down and that `reset` gives a full bucket back.
#[test]
fn token_bucket_remaining_and_reset() {
    let limiter = TokenBucket::new(1, 3, Duration::from_secs(60));
    assert_eq!(limiter.remaining("user"), 3);
    limiter.try_acquire("user");
    assert_eq!(limiter.remaining("user"), 2);
    limiter.reset("user");
    assert_eq!(limiter.remaining("user"), 3);
}

/// Checks that time left over from a partial interval carries into the next refill.
///
/// The bucket holds 2 and refills 1 every 200 ms. After both tokens are
/// used, a request at 300 ms gets the one refilled token and leaves 100 ms
/// of progress towards the next. At 450 ms, 250 ms have passed since that
/// last whole interval, so another token is due. Discarding the partial
/// interval would count only 150 ms and refuse the request. (Progress is
/// only carried while the bucket isn't full; a full bucket can't store more.)
#[test]
fn token_bucket_carries_partial_intervals() {
    let limiter = TokenBucket::new(1, 2, Duration::from_millis(200));
    assert!(limiter.try_acquire("user"));
    assert!(limiter.try_acquire("user"));

    sleep(Duration::from_millis(300));
    assert!(limiter.try_acquire("user"));
    assert!(!limiter.try_acquire("user"));

    sleep(Duration::from_millis(150));
    assert!(limiter.try_acquire("user"));
}

/// Checks that `retry_after` is `None` once a refill is due, even before anything is charged.
#[test]
fn token_bucket_retry_after_counts_due_refills() {
    let limiter = TokenBucket::new(1, 1, Duration::from_millis(50));
    limiter.try_acquire("user");
    assert!(limiter.retry_after("user").is_some());

    sleep(Duration::from_millis(80));

    assert_eq!(limiter.retry_after("user"), None);
}

/// Checks that a zero refill interval is rejected when the limiter is built.
#[test]
#[should_panic(expected = "interval must not be zero")]
fn token_bucket_rejects_zero_interval() {
    TokenBucket::new(1, 1, Duration::ZERO);
}

/// Checks that pruning removes only buckets that have been idle long enough.
#[test]
fn token_bucket_prunes_only_idle_keys() {
    let limiter = TokenBucket::new(1, 1, Duration::from_millis(10));
    limiter.try_acquire("old");
    sleep(Duration::from_millis(60));
    limiter.try_acquire("fresh");

    assert_eq!(limiter.prune_idle(Duration::from_millis(40)), 1);
    assert_eq!(limiter.tracked_keys(), 1);
    assert_eq!(limiter.remaining("fresh"), 0);
}

/// Checks that a window allows its limit, refuses the next request, and allows again once it slides.
#[test]
fn sliding_window_allows_limit_then_slides() {
    let limiter = SlidingWindow::new(2, Duration::from_millis(100));
    assert!(limiter.try_acquire("test"));
    assert!(limiter.try_acquire("test"));
    assert!(!limiter.try_acquire("test"));

    sleep(Duration::from_millis(150));

    assert!(limiter.try_acquire("test"));
}

/// Checks that each key has its own window and that `remaining` counts down.
#[test]
fn sliding_window_keys_and_remaining() {
    let limiter = SlidingWindow::new(2, Duration::from_secs(60));
    assert_eq!(limiter.remaining("user1"), 2);
    limiter.try_acquire("user1");
    limiter.try_acquire("user1");
    assert_eq!(limiter.remaining("user1"), 0);
    assert!(!limiter.try_acquire("user1"));
    assert!(limiter.try_acquire("user2"));
    assert!(limiter.retry_after("user1").is_some());
}

/// Checks that pruning removes windows with no requests left in them.
#[test]
fn sliding_window_prunes_empty_windows() {
    let limiter = SlidingWindow::new(5, Duration::from_millis(30));
    limiter.try_acquire("old");
    sleep(Duration::from_millis(50));
    limiter.try_acquire("fresh");

    assert_eq!(limiter.prune_idle(), 1);
    assert_eq!(limiter.tracked_keys(), 1);
}

/// Checks that asking about an unknown key never creates state for it.
#[test]
fn read_methods_do_not_create_keys() {
    let bucket = TokenBucket::new(1, 1, Duration::from_secs(1));
    let window = SlidingWindow::new(1, Duration::from_secs(1));
    let login = LoginRateLimiter::new();

    bucket.remaining("ghost");
    bucket.retry_after("ghost");
    window.remaining("ghost");
    window.retry_after("ghost");
    login.check_status("ghost");
    login.failure_count("ghost");
    login.lockout_remaining("ghost");
    login.unlock("ghost");

    assert_eq!(bucket.tracked_keys(), 0);
    assert_eq!(window.tracked_keys(), 0);
    assert_eq!(login.tracked_keys(), 0);
}

/// Checks that a key is locked out after the configured number of failures.
#[test]
fn login_limiter_locks_out_after_max_attempts() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(3)
        .lockout_duration(Duration::from_secs(60))
        .build();
    for _ in 0..2 {
        limiter.record_failure("user");
        assert!(limiter.check_allowed("user"));
    }
    limiter.record_failure("user");

    assert!(!limiter.check_allowed("user"));
    assert!(matches!(
        limiter.check_status("user"),
        LoginStatus::LockedOut { retry_after } if (1..=60).contains(&retry_after)
    ));
}

/// Checks that a successful login clears earlier failures.
#[test]
fn login_limiter_success_resets() {
    let limiter = LoginRateLimiter::builder().max_attempts(3).build();
    limiter.record_failure("user");
    limiter.record_failure("user");
    limiter.record_success("user");

    assert_eq!(limiter.failure_count("user"), 0);
    assert_eq!(limiter.remaining_attempts("user"), 3);
    assert_eq!(limiter.tracked_keys(), 0);
}

/// Checks that locking out one key leaves others unaffected.
#[test]
fn login_limiter_keys_are_independent() {
    let limiter = LoginRateLimiter::builder().max_attempts(2).build();
    limiter.record_failure("user1");
    limiter.record_failure("user1");

    assert!(!limiter.check_allowed("user1"));
    assert!(limiter.check_allowed("user2"));
}

/// Checks that `unlock` lifts a lockout by hand.
#[test]
fn login_limiter_manual_unlock() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(1)
        .lockout_duration(Duration::from_secs(60))
        .build();
    limiter.record_failure("user");
    assert!(limiter.is_locked("user"));

    limiter.unlock("user");

    assert!(!limiter.is_locked("user"));
}

/// Checks that a second lockout lasts longer than the first with backoff on.
#[test]
fn login_limiter_backs_off_exponentially() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(1)
        .lockout_duration(Duration::from_millis(10))
        .exponential_backoff(true)
        .max_lockout_duration(Duration::from_secs(1))
        .build();
    limiter.record_failure("user");
    assert!(!limiter.check_allowed("user"));

    sleep(Duration::from_millis(15));
    assert!(limiter.check_allowed("user"));
    assert_eq!(limiter.failure_count("user"), 0);

    limiter.record_failure("user");
    let remaining = limiter.lockout_remaining("user").expect("locked again");
    assert!(remaining > Duration::from_millis(10));
}

/// Checks that pruning keeps keys that are locked out or failed recently.
#[test]
fn login_limiter_keeps_locked_and_recent_keys() {
    let limiter = LoginRateLimiter::builder()
        .max_attempts(1)
        .lockout_duration(Duration::from_secs(60))
        .build();
    limiter.record_failure("locked");
    limiter.record_failure("recent");
    limiter.unlock("recent");

    assert_eq!(limiter.prune_idle(Duration::from_secs(60)), 1);
    assert!(limiter.is_locked("locked"));
}

/// Checks that charging, resetting and pruning from many threads at once never panics.
#[test]
fn concurrent_reset_never_panics() {
    let token = Arc::new(TokenBucket::new(1000, 1000, Duration::from_secs(1)));
    let window = Arc::new(SlidingWindow::new(1000, Duration::from_secs(1)));
    let login = Arc::new(LoginRateLimiter::new());

    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let token = token.clone();
            let window = window.clone();
            let login = login.clone();
            std::thread::spawn(move || {
                for _ in 0..2000 {
                    if worker % 2 == 0 {
                        token.try_acquire("shared");
                        window.try_acquire("shared");
                        login.record_failure("shared");
                    } else {
                        token.reset("shared");
                        window.reset("shared");
                        login.prune_idle(Duration::ZERO);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker
            .join()
            .expect("a limiter panicked under concurrent reset");
    }
}
