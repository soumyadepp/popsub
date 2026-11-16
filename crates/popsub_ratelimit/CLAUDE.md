# popsub_ratelimit

Keyed rate limiters used by the transport: a token bucket for publishing, a sliding window for registration, and a login limiter with lockouts and exponential backoff.

## Files

- `src/keyed.rs`: `KeyedState<T>`, the per-key store all three limiters use. `update` gets or creates a key's state and changes it under its lock, `read` and `update_existing` never create keys, `retain` prunes, and poisoned locks are recovered instead of panicking.
- `src/token_bucket.rs`: `TokenBucket`, refills `rate` tokens per whole `interval` up to `capacity`, carrying partial intervals forward while the bucket isn't full.
- `src/sliding_window.rs`: `SlidingWindow`, at most `max_requests` per `window_size`.
- `src/login_limiter.rs`: `LoginRateLimiter`, counts failures per key and locks the key out, doubling the lockout up to a maximum. `LoginRateLimiterConfig` is exported for `with_config`.
- `src/lib.rs`: the synchronous `RateLimiter` trait implemented by the first two.

## Invariants

- Everything is synchronous. Don't add `async` to these methods; nothing in them waits.
- All per-key state goes through `KeyedState`. Don't reach into a `DashMap` directly; that is where the get-then-lookup race and the poisoning panics used to live.
- Read methods (`remaining`, `retry_after`, `check_status`, `failure_count`, `lockout_remaining`) must never create state for an unknown key.
- Every charged key stays in memory until pruned, so the transport calls `prune_idle` every minute.
- A `TokenBucket` interval of zero is rejected at construction.

## Gotchas

- `record_success` forgets the key entirely, including its backoff count. `unlock` keeps the backoff count.
- Keys are plain strings chosen by the caller. The transport uses `address|username` for logins, the address for registration and the username for publishing.

## Tests

- `make test-crate CRATE=popsub_ratelimit`. Timing tests sleep for at most a few hundred milliseconds; `concurrent_reset_never_panics` uses plain threads.
