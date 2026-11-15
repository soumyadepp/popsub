# popsub_ratelimit

Keyed rate limiters used by the transport: a token bucket for publishing, a sliding window for registration, and a login limiter with lockouts and exponential backoff.

## Files

- `src/token_bucket.rs`: `TokenBucket`, refills `rate` tokens per `interval` up to `capacity`.
- `src/sliding_window.rs`: `SlidingWindow`, at most `max_requests` per `window_size`.
- `src/login_limiter.rs`: `LoginRateLimiter`, counts failures per key and locks the key out, doubling the lockout up to a maximum.
- `src/lib.rs`: the `RateLimiter` trait implemented by the first two.

## Invariants

- Entries are created on first use, so every limiter must be pruned (`prune_idle`) or memory grows with every key ever seen. The transport prunes every minute.
- Get-or-create must use the reference returned by `entry(...).or_insert_with(...).downgrade()`. Looking the key up again after inserting races with `reset` and can panic.
- `retain` closures receive `&mut` values: use `get_mut()` on the inner mutex, not `lock()` (clippy rejects the latter).
- Counts returned by `prune_idle` are approximate when other tasks insert at the same time; never subtract without saturating.

## Gotchas

- The trait methods are `async` but never await, so callers pay for futures they don't need.
- Keys are plain strings chosen by the caller. The transport uses `address|username` for logins, the address for registration and the username for publishing.

## Tests

- `make test-crate CRATE=popsub_ratelimit`. `concurrent_reset_never_panics` runs on a multi-threaded runtime; keep `rt-multi-thread` in dev-dependencies.
