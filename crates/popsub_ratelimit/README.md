# popsub_ratelimit

A flexible, reusable rate limiting library for PopSub.

## Features

- **Token Bucket**: Classic rate limiting with burst support
- **Sliding Window**: Smooth rate limiting without allowing bursts
- **Login Rate Limiter**: Specialized for authentication with lockout support

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
popsub_ratelimit = { path = "../popsub_ratelimit" }
```

## Usage

### Token Bucket

Best for APIs where you want to allow occasional bursts:

```rust
use popsub_ratelimit::{RateLimiter, TokenBucket};
use std::time::Duration;

// 10 requests per second, burst of 20
let limiter = TokenBucket::new(10, 20, Duration::from_secs(1));

// Or use presets
let limiter = TokenBucket::per_second(100);
let limiter = TokenBucket::per_minute(1000);

// Check rate limit
if limiter.try_acquire("user_123").await {
    // Process request
} else {
    // Return 429 Too Many Requests
    let retry_after = limiter.retry_after("user_123").await;
}
```

### Sliding Window

Best for strict rate limiting without bursts:

```rust
use popsub_ratelimit::{RateLimiter, SlidingWindow};
use std::time::Duration;

// Max 100 requests per minute
let limiter = SlidingWindow::new(100, Duration::from_secs(60));

// Or use presets
let limiter = SlidingWindow::per_second(10);
let limiter = SlidingWindow::per_minute(100);
```

### Login Rate Limiter

Specialized for authentication with account lockout:

```rust
use popsub_ratelimit::{LoginRateLimiter, LoginStatus};
use std::time::Duration;

let limiter = LoginRateLimiter::builder()
    .max_attempts(5)                              // Lock after 5 failures
    .lockout_duration(Duration::from_secs(300))   // 5 minute lockout
    .exponential_backoff(true)                    // Double lockout each time
    .max_lockout_duration(Duration::from_secs(3600)) // Max 1 hour
    .build();

// Before login attempt
match limiter.check_status("username").await {
    LoginStatus::Allowed => {
        // Proceed with login
    }
    LoginStatus::LockedOut { retry_after } => {
        return Err(format!("Too many attempts. Try again in {} seconds", retry_after));
    }
}

// After failed login
limiter.record_failure("username").await;

// After successful login (resets counter)
limiter.record_success("username").await;

// Admin: manually unlock account
limiter.unlock("username").await;
```

## Common Trait

All rate limiters implement the `RateLimiter` trait:

```rust
pub trait RateLimiter: Send + Sync {
    async fn try_acquire(&self, key: &str) -> bool;
    async fn remaining(&self, key: &str) -> u64;
    async fn reset(&self, key: &str);
    async fn retry_after(&self, key: &str) -> Option<u64>;
}
```

## Thread Safety

All rate limiters are thread-safe and can be shared across tasks using `Arc`:

```rust
use std::sync::Arc;

let limiter = Arc::new(TokenBucket::per_second(100));

// Clone for each task
let limiter_clone = limiter.clone();
tokio::spawn(async move {
    limiter_clone.try_acquire("key").await;
});
```

## Algorithm Comparison

| Algorithm      | Burst Support | Memory             | Use Case          |
| -------------- | ------------- | ------------------ | ----------------- |
| Token Bucket   | Yes           | O(keys)            | API rate limiting |
| Sliding Window | No            | O(keys × requests) | Strict limiting   |
| Login Limiter  | N/A           | O(keys)            | Authentication    |

## License

MIT
