//! `LoginRateLimiter`, which counts failed logins per key and locks the key out after too many.
//!
//! After `max_attempts` failures a key is locked out for `lockout_duration`.
//! With exponential backoff on, each further lockout doubles that duration
//! up to `max_lockout_duration`. A successful login clears the failures and
//! the backoff history. The transport keys it on `address|username`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::{info, warn};

/// Result of checking whether a key may attempt a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStatus {
    /// The key is not locked out.
    Allowed,
    /// The key is locked out.
    LockedOut {
        /// Whole seconds until the lockout ends, at least 1.
        retry_after: u64,
    },
}

/// Failure and lockout history for one key.
#[derive(Default)]
struct LoginState {
    /// Failed attempts in the current cycle. Reset by the first failure after a lockout ends.
    failures: u32,
    /// Lockouts so far, used as the exponent for backoff. Cleared only by a successful login.
    lockout_count: u32,
    /// Time of the last failure, used by `prune_idle` to decide whether the key is idle.
    last_failure: Option<Instant>,
    /// When the current lockout ends, if the key has been locked out.
    locked_until: Option<Instant>,
}

/// Settings for `LoginRateLimiter`.
#[derive(Debug, Clone)]
pub struct LoginRateLimiterConfig {
    /// Failed attempts that trigger a lockout.
    pub max_attempts: u32,
    /// Length of the first lockout, and of every lockout without backoff.
    pub lockout_duration: Duration,
    /// Whether each further lockout doubles in length.
    pub exponential_backoff: bool,
    /// Upper limit on a lockout's length when backoff is on.
    pub max_lockout_duration: Duration,
}

impl Default for LoginRateLimiterConfig {
    /// Returns 5 attempts, a 5 minute lockout, backoff on and a 1 hour maximum lockout.
    ///
    /// Returns:
    ///     The default configuration.
    fn default() -> Self {
        Self {
            max_attempts: 5,
            lockout_duration: Duration::from_secs(300),
            exponential_backoff: true,
            max_lockout_duration: Duration::from_secs(3600),
        }
    }
}

/// Builder for `LoginRateLimiter`, starting from `LoginRateLimiterConfig::default()`.
pub struct LoginRateLimiterBuilder {
    /// The configuration being built.
    config: LoginRateLimiterConfig,
}

impl LoginRateLimiterBuilder {
    /// Creates a builder holding the default configuration.
    ///
    /// Returns:
    ///     A new builder.
    pub fn new() -> Self {
        Self {
            config: LoginRateLimiterConfig::default(),
        }
    }

    /// Sets how many failed attempts trigger a lockout.
    ///
    /// Args:
    ///     attempts: The number of failures allowed before the key is locked out.
    ///
    /// Returns:
    ///     The builder, for chaining.
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.config.max_attempts = attempts;
        self
    }

    /// Sets the length of the first lockout.
    ///
    /// Args:
    ///     duration: How long a key stays locked out the first time, and every time without backoff.
    ///
    /// Returns:
    ///     The builder, for chaining.
    pub fn lockout_duration(mut self, duration: Duration) -> Self {
        self.config.lockout_duration = duration;
        self
    }

    /// Turns exponential backoff on or off.
    ///
    /// Args:
    ///     enabled: Whether each further lockout doubles in length.
    ///
    /// Returns:
    ///     The builder, for chaining.
    pub fn exponential_backoff(mut self, enabled: bool) -> Self {
        self.config.exponential_backoff = enabled;
        self
    }

    /// Sets the upper limit on a lockout's length when backoff is on.
    ///
    /// Args:
    ///     duration: The longest a single lockout can last.
    ///
    /// Returns:
    ///     The builder, for chaining.
    pub fn max_lockout_duration(mut self, duration: Duration) -> Self {
        self.config.max_lockout_duration = duration;
        self
    }

    /// Creates the limiter from the configuration built so far.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn build(self) -> LoginRateLimiter {
        LoginRateLimiter {
            config: self.config,
            states: DashMap::new(),
        }
    }
}

impl Default for LoginRateLimiterBuilder {
    /// Creates a builder holding the default configuration.
    ///
    /// Returns:
    ///     A new builder.
    fn default() -> Self {
        Self::new()
    }
}

/// Login rate limiter that locks a key out after repeated failures.
///
/// Callers check the key before each attempt and record the outcome after
/// it. Every method that takes a key starts tracking it if it is new, so
/// call `prune_idle` periodically.
///
/// Example:
///
/// ```rust,ignore
/// use popsub_ratelimit::LoginRateLimiter;
/// use std::time::Duration;
///
/// let limiter = LoginRateLimiter::builder()
///     .max_attempts(5)
///     .lockout_duration(Duration::from_secs(300))
///     .exponential_backoff(true)
///     .build();
///
/// let allowed = limiter.check_allowed("username").await;
/// limiter.record_failure("username").await;
/// limiter.record_success("username").await;
/// ```
pub struct LoginRateLimiter {
    /// Attempt limit and lockout settings.
    config: LoginRateLimiterConfig,
    /// Failure and lockout history by key.
    states: DashMap<String, Mutex<LoginState>>,
}

impl LoginRateLimiter {
    /// Creates a limiter with the default configuration.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Creates a builder for a custom configuration.
    ///
    /// Returns:
    ///     A builder holding the default configuration.
    pub fn builder() -> LoginRateLimiterBuilder {
        LoginRateLimiterBuilder::new()
    }

    /// Creates a limiter with the given configuration.
    ///
    /// Args:
    ///     config: The attempt limit and lockout settings.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    pub fn with_config(config: LoginRateLimiterConfig) -> Self {
        Self {
            config,
            states: DashMap::new(),
        }
    }

    /// Returns the state for `key`, inserting an empty one if the key is new.
    ///
    /// Uses the map's entry API so a concurrent `prune_idle` can't remove the
    /// key between the lookup and the insert.
    ///
    /// Args:
    ///     key: The key to look up.
    ///
    /// Returns:
    ///     A read guard on the key's map entry.
    fn get_or_create_state(
        &self,
        key: &str,
    ) -> dashmap::mapref::one::Ref<'_, String, Mutex<LoginState>> {
        self.states
            .entry(key.to_string())
            .or_insert_with(|| Mutex::new(LoginState::default()))
            .downgrade()
    }

    /// Forgets keys that are not locked out and have had no failure for
    /// `max_idle`.
    ///
    /// Without this, every username or address ever tried stays in memory.
    /// Pruning also forgets the exponential backoff history for that key, so
    /// choose `max_idle` at least as long as the maximum lockout. Panics if a
    /// key's mutex is poisoned.
    ///
    /// Args:
    ///     max_idle: How long a key must have been quiet before it is removed.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     tasks add keys at the same time.
    pub fn prune_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        let before = self.states.len();
        self.states.retain(|_, state| {
            let state = state.get_mut().unwrap();
            let locked = state.locked_until.is_some_and(|until| now < until);
            let recent = state
                .last_failure
                .is_some_and(|last| now.duration_since(last) < max_idle);
            locked || recent
        });
        before.saturating_sub(self.states.len())
    }

    /// Reports how many keys the limiter currently holds state for.
    ///
    /// Returns:
    ///     The number of tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.states.len()
    }

    /// Reports whether `key` may attempt a login now.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `true` if the key is not locked out, `false` if it is.
    pub async fn check_allowed(&self, key: &str) -> bool {
        matches!(self.check_status(key).await, LoginStatus::Allowed)
    }

    /// Reports whether `key` is locked out and for how long.
    ///
    /// Panics if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `LoginStatus::Allowed`, or `LoginStatus::LockedOut` with the
    ///     seconds left, rounded down and at least 1.
    pub async fn check_status(&self, key: &str) -> LoginStatus {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();

        if let Some(locked_until) = state.locked_until {
            let now = Instant::now();
            if now < locked_until {
                let retry_after = (locked_until - now).as_secs().max(1);
                return LoginStatus::LockedOut { retry_after };
            }
        }

        LoginStatus::Allowed
    }

    /// Records a failed login for `key` and locks it out once it reaches `max_attempts`.
    ///
    /// If a previous lockout has ended, the failure count starts again from
    /// zero but the lockout count is kept, so each new lockout cycle gets a
    /// fresh `max_attempts` while backoff still grows. Once the count reaches
    /// `max_attempts` the lockout count goes up and the key is locked for
    /// `lockout_duration`, or with backoff for `lockout_duration` times
    /// 2 to the power of (lockout count minus 1), capped at
    /// `max_lockout_duration`. A failure recorded while the key is still
    /// locked also counts, and starts a new lockout from now (a longer one
    /// when backoff is on). Panics
    /// if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key the failed attempt belongs to.
    pub async fn record_failure(&self, key: &str) {
        let state_ref = self.get_or_create_state(key);
        let mut state = state_ref.lock().unwrap();
        let now = Instant::now();

        if let Some(locked_until) = state.locked_until
            && now >= locked_until
        {
            state.locked_until = None;
            state.failures = 0;
        }

        state.failures += 1;
        state.last_failure = Some(now);

        if state.failures >= self.config.max_attempts {
            state.lockout_count += 1;

            let lockout_duration = if self.config.exponential_backoff {
                let base_millis = self.config.lockout_duration.as_millis() as u64;
                let exponential_millis = base_millis
                    .saturating_mul(2_u64.saturating_pow(state.lockout_count.saturating_sub(1)));
                let max_millis = self.config.max_lockout_duration.as_millis() as u64;
                Duration::from_millis(exponential_millis.min(max_millis))
            } else {
                self.config.lockout_duration
            };

            state.locked_until = Some(now + lockout_duration);
            warn!(
                key = %key,
                failures = state.failures,
                lockout_secs = lockout_duration.as_secs(),
                "Account locked due to too many failed attempts"
            );
        }
    }

    /// Records a successful login for `key`.
    ///
    /// Clears the failure count, the lockout count and any lockout, so the
    /// next lockout starts again at `lockout_duration`. Panics if the key's
    /// mutex is poisoned.
    ///
    /// Args:
    ///     key: The key that logged in.
    pub async fn record_success(&self, key: &str) {
        let state_ref = self.get_or_create_state(key);
        let mut state = state_ref.lock().unwrap();

        if state.failures > 0 || state.lockout_count > 0 {
            info!(key = %key, previous_failures = state.failures, lockout_count = state.lockout_count, "Login successful, resetting failure count");
        }

        state.failures = 0;
        state.lockout_count = 0;
        state.last_failure = None;
        state.locked_until = None;
    }

    /// Reports the failures recorded for `key` in the current cycle.
    ///
    /// After a lockout ends the old count is still reported until the next
    /// failure resets it. Panics if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The number of failed attempts.
    pub async fn failure_count(&self, key: &str) -> u32 {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();
        state.failures
    }

    /// Reports how many more failures `key` can have before it is locked out.
    ///
    /// After a lockout ends this still reports 0 until the next failure
    /// starts a new cycle. Panics if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `max_attempts` minus the current failure count, at least 0.
    pub async fn remaining_attempts(&self, key: &str) -> u32 {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();
        self.config.max_attempts.saturating_sub(state.failures)
    }

    /// Lifts any lockout on `key` and clears its failure count, as an admin action.
    ///
    /// The lockout count is kept, so a later lockout still uses the longer
    /// backoff duration. Panics if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key to unlock.
    pub async fn unlock(&self, key: &str) {
        let state_ref = self.get_or_create_state(key);
        let mut state = state_ref.lock().unwrap();
        state.failures = 0;
        state.last_failure = None;
        state.locked_until = None;
        info!(key = %key, "Account manually unlocked");
    }

    /// Reports whether `key` is locked out now.
    ///
    /// Panics if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `true` if a lockout is in force.
    pub async fn is_locked(&self, key: &str) -> bool {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();

        if let Some(locked_until) = state.locked_until {
            Instant::now() < locked_until
        } else {
            false
        }
    }

    /// Reports how long the lockout on `key` has left.
    ///
    /// Panics if the key's mutex is poisoned.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The time left, or `None` if the key is not locked out.
    pub async fn lockout_remaining(&self, key: &str) -> Option<Duration> {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();

        if let Some(locked_until) = state.locked_until {
            let now = Instant::now();
            if now < locked_until {
                return Some(locked_until - now);
            }
        }
        None
    }
}

impl Default for LoginRateLimiter {
    /// Creates a limiter with the default configuration.
    ///
    /// Returns:
    ///     A limiter that tracks no keys yet.
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `LoginRateLimiter`.

    use super::*;

    /// Checks that a lockout on one key does not affect another key.
    #[tokio::test]
    async fn test_separate_keys() {
        let limiter = LoginRateLimiter::builder().max_attempts(2).build();

        limiter.record_failure("user1").await;
        limiter.record_failure("user1").await;

        assert!(!limiter.check_allowed("user1").await);
        assert!(limiter.check_allowed("user2").await);
    }

    /// Checks that `remaining_attempts` starts at `max_attempts` and drops by one per failure.
    #[tokio::test]
    async fn test_remaining_attempts() {
        let limiter = LoginRateLimiter::builder().max_attempts(5).build();

        assert_eq!(limiter.remaining_attempts("user").await, 5);
        limiter.record_failure("user").await;
        assert_eq!(limiter.remaining_attempts("user").await, 4);
    }

    /// Checks that `unlock` lifts an active lockout.
    #[tokio::test]
    async fn test_manual_unlock() {
        let limiter = LoginRateLimiter::builder()
            .max_attempts(1)
            .lockout_duration(Duration::from_secs(3600))
            .build();

        limiter.record_failure("user").await;
        assert!(!limiter.check_allowed("user").await);

        limiter.unlock("user").await;
        assert!(limiter.check_allowed("user").await);
    }

    /// Checks that the second lockout lasts longer than the first when backoff is on.
    #[tokio::test]
    async fn test_exponential_backoff() {
        let limiter = LoginRateLimiter::builder()
            .max_attempts(1)
            .lockout_duration(Duration::from_millis(10))
            .exponential_backoff(true)
            .max_lockout_duration(Duration::from_secs(100))
            .build();

        limiter.record_failure("user").await;
        assert!(!limiter.check_allowed("user").await);

        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(limiter.check_allowed("user").await);

        limiter.record_failure("user").await;
        let remaining = limiter.lockout_remaining("user").await;
        assert!(remaining.is_some());
        assert!(remaining.unwrap() > Duration::from_millis(10));
    }

    /// Checks that `check_status` reports `Allowed`, then `LockedOut` with a retry time within the lockout.
    #[tokio::test]
    async fn test_login_status_details() {
        let limiter = LoginRateLimiter::builder()
            .max_attempts(1)
            .lockout_duration(Duration::from_secs(60))
            .build();

        assert_eq!(limiter.check_status("user").await, LoginStatus::Allowed);

        limiter.record_failure("user").await;

        match limiter.check_status("user").await {
            LoginStatus::LockedOut { retry_after } => {
                assert!(retry_after > 0);
                assert!(retry_after <= 60);
            }
            _ => panic!("Expected LockedOut status"),
        }
    }
}
