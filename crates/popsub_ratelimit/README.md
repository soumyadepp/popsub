# popsub_ratelimit

Per-key rate limiters for PopSub: token bucket, sliding window and login lockout.

## Features

- **Token Bucket**: Classic rate limiting with burst support
- **Sliding Window**: Smooth rate limiting without allowing bursts
- **Login Rate Limiter**: Specialized for authentication with lockout support

Every method is synchronous; nothing here needs an async runtime.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
popsub_ratelimit = { path = "../popsub_ratelimit" }
```

## Usage

### Token Bucket

Best for APIs where you want to allow occasional bursts. `TokenBucket::new(rate, capacity, interval)` adds `rate` tokens every `interval`, up to `capacity`. This one allows 10 requests per second with a burst of 20; `per_second` and `per_minute` set the burst to twice the rate.

```rust
use popsub_ratelimit::{RateLimiter, TokenBucket};
use std::time::Duration;

let limiter = TokenBucket::new(10, 20, Duration::from_secs(1));
let per_second = TokenBucket::per_second(100);
let per_minute = TokenBucket::per_minute(1000);

if !limiter.try_acquire("user_123") {
    let retry_after: Option<u64> = limiter.retry_after("user_123");
    println!("rate limited, retry in {retry_after:?} seconds");
}
```

### Sliding Window

Best for strict rate limiting without bursts. `SlidingWindow::new(max_requests, window_size)` allows at most `max_requests` in any window of that length.

```rust
use popsub_ratelimit::{RateLimiter, SlidingWindow};
use std::time::Duration;

let limiter = SlidingWindow::new(100, Duration::from_secs(60));
let per_second = SlidingWindow::per_second(10);
let per_minute = SlidingWindow::per_minute(100);

let allowed = limiter.try_acquire("203.0.113.7");
let left = limiter.remaining("203.0.113.7");
```

### Login Rate Limiter

Specialized for authentication with account lockout. This one locks a key out for 5 minutes after 5 failures, doubles the lockout each time it happens again, and never locks out for more than an hour. Check the status before a login, record a failure after a wrong password, and record a success to forget the key. `unlock` lifts a lockout by hand.

```rust
use popsub_ratelimit::{LoginRateLimiter, LoginStatus};
use std::time::Duration;

fn log_in(limiter: &LoginRateLimiter, username: &str, password_ok: bool) -> Result<(), String> {
    if let LoginStatus::LockedOut { retry_after } = limiter.check_status(username) {
        return Err(format!("Too many attempts. Try again in {retry_after} seconds"));
    }
    if password_ok {
        limiter.record_success(username);
        Ok(())
    } else {
        limiter.record_failure(username);
        Err("wrong password".to_string())
    }
}

let limiter = LoginRateLimiter::builder()
    .max_attempts(5)
    .lockout_duration(Duration::from_secs(300))
    .exponential_backoff(true)
    .max_lockout_duration(Duration::from_secs(3600))
    .build();

let _ = log_in(&limiter, "alice", false);
limiter.unlock("alice");
```

## Common Trait

`TokenBucket` and `SlidingWindow` implement the `RateLimiter` trait. `LoginRateLimiter` does not; it has its own methods for failures and lockouts.

```rust
pub trait RateLimiter: Send + Sync {
    fn try_acquire(&self, key: &str) -> bool;
    fn remaining(&self, key: &str) -> u64;
    fn reset(&self, key: &str);
    fn retry_after(&self, key: &str) -> Option<u64>;
}
```

## Thread Safety

All rate limiters are thread-safe and can be shared across threads and tasks using `Arc`:

```rust
use popsub_ratelimit::{RateLimiter, TokenBucket};
use std::sync::Arc;

let limiter = Arc::new(TokenBucket::per_second(100));

let limiter_clone = limiter.clone();
std::thread::spawn(move || {
    limiter_clone.try_acquire("key");
});
```

Every key that has been charged stays in memory until it is reset or pruned, so call `prune_idle` now and then (the PopSub server does it every minute).

## Algorithm Comparison

| Algorithm      | Burst Support | Memory             | Use Case          |
| -------------- | ------------- | ------------------ | ----------------- |
| Token Bucket   | Yes           | O(keys)            | API rate limiting |
| Sliding Window | No            | O(keys × requests) | Strict limiting   |
| Login Limiter  | N/A           | O(keys)            | Authentication    |

## License

Apache-2.0
