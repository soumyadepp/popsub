//! `LoginRateLimiter`, which counts failed logins per key and locks the key out after too many.
//!
//! After `max_attempts` failures a key is locked out for `lockout_duration`.
//! With exponential backoff on, each further lockout doubles that duration
//! up to `max_lockout_duration`. A successful login forgets the key entirely,
//! including its backoff history. The transport keys one limiter on
//! `address|username` and another on the address alone.

use popsub_utils::pii::pii;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::keyed::KeyedState;

/// Whether a key may attempt a login now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStatus {
    /// The key may attempt a login.
    Allowed,
    /// The key is locked out.
    LockedOut {
        /// Whole seconds until the lockout ends, at least 1.
        retry_after: u64,
    },
}

/// One key's failure history.
#[derive(Default)]
struct LoginState {
    /// Failures since the last lockout ended or the last success.
    failures: u32,
    /// How many times the key has been locked out, for backoff.
    lockout_count: u32,
    /// When the last failure happened, used to find idle keys.
    last_failure: Option<Instant>,
    /// When the current lockout ends, if there is one.
    locked_until: Option<Instant>,
}

impl LoginState {
    /// The lockout end time if it is still in the future.
    ///
    /// Args:
    ///     now: The current time.
    ///
    /// Returns:
    ///     When the active lockout ends, or `None` when there isn't one.
    fn active_lockout(&self, now: Instant) -> Option<Instant> {
        self.locked_until.filter(|&until| now < until)
    }

    /// Failures that still count towards the next lockout.
    ///
    /// A lockout that has ended wipes the slate, even before the next
    /// failure is recorded.
    ///
    /// Args:
    ///     now: The current time.
    ///
    /// Returns:
    ///     The failure count in effect now.
    fn effective_failures(&self, now: Instant) -> u32 {
        match self.locked_until {
            Some(until) if now >= until => 0,
            _ => self.failures,
        }
    }
}

/// Settings for a `LoginRateLimiter`.
#[derive(Debug, Clone)]
pub struct LoginRateLimiterConfig {
    /// Failures allowed before a lockout.
    pub max_attempts: u32,
    /// Length of the first lockout.
    pub lockout_duration: Duration,
    /// Whether each further lockout doubles in length.
    pub exponential_backoff: bool,
    /// The longest a lockout can grow to with backoff.
    pub max_lockout_duration: Duration,
}

impl Default for LoginRateLimiterConfig {
    /// Builds the defaults: 5 attempts, a 5 minute lockout, backoff on, capped at 1 hour.
    ///
    /// Returns:
    ///     The default config.
    fn default() -> Self {
        Self {
            max_attempts: 5,
            lockout_duration: Duration::from_secs(300),
            exponential_backoff: true,
            max_lockout_duration: Duration::from_secs(3600),
        }
    }
}

/// Builds a `LoginRateLimiter` one setting at a time, starting from the defaults.
#[derive(Default)]
pub struct LoginRateLimiterBuilder {
    /// The settings so far.
    config: LoginRateLimiterConfig,
}

impl LoginRateLimiterBuilder {
    /// Starts from `LoginRateLimiterConfig::default()`.
    ///
    /// Returns:
    ///     A builder with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how many failures are allowed before a lockout.
    ///
    /// Args:
    ///     attempts: The failure limit.
    ///
    /// Returns:
    ///     The builder.
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.config.max_attempts = attempts;
        self
    }

    /// Sets the length of the first lockout.
    ///
    /// Args:
    ///     duration: The lockout length.
    ///
    /// Returns:
    ///     The builder.
    pub fn lockout_duration(mut self, duration: Duration) -> Self {
        self.config.lockout_duration = duration;
        self
    }

    /// Turns doubling of repeated lockouts on or off.
    ///
    /// Args:
    ///     enabled: Whether each further lockout doubles in length.
    ///
    /// Returns:
    ///     The builder.
    pub fn exponential_backoff(mut self, enabled: bool) -> Self {
        self.config.exponential_backoff = enabled;
        self
    }

    /// Sets the longest a lockout can grow to with backoff.
    ///
    /// Args:
    ///     duration: The cap.
    ///
    /// Returns:
    ///     The builder.
    pub fn max_lockout_duration(mut self, duration: Duration) -> Self {
        self.config.max_lockout_duration = duration;
        self
    }

    /// Builds the limiter.
    ///
    /// Returns:
    ///     A limiter with these settings and no keys.
    pub fn build(self) -> LoginRateLimiter {
        LoginRateLimiter::with_config(self.config)
    }
}

/// Counts failed logins per key and locks keys out after too many.
///
/// Call `check_status` before an attempt, `record_failure` after a failed
/// one and `record_success` after a successful one. The read methods never
/// create state for a key they haven't seen.
///
/// Example:
///
/// ```rust
/// use popsub_ratelimit::LoginRateLimiter;
/// use std::time::Duration;
///
/// let limiter = LoginRateLimiter::builder()
///     .max_attempts(2)
///     .lockout_duration(Duration::from_secs(300))
///     .build();
/// limiter.record_failure("alice");
/// limiter.record_failure("alice");
/// assert!(!limiter.check_allowed("alice"));
/// ```
pub struct LoginRateLimiter {
    /// The limits.
    config: LoginRateLimiterConfig,
    /// Every key's failure history.
    states: KeyedState<LoginState>,
}

impl Default for LoginRateLimiter {
    /// Builds a limiter with `LoginRateLimiterConfig::default()`.
    ///
    /// Returns:
    ///     A limiter with no keys.
    fn default() -> Self {
        Self::with_config(LoginRateLimiterConfig::default())
    }
}

impl LoginRateLimiter {
    /// Builds a limiter with the default settings.
    ///
    /// Returns:
    ///     A limiter with no keys.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a builder from the default settings.
    ///
    /// Returns:
    ///     A builder.
    pub fn builder() -> LoginRateLimiterBuilder {
        LoginRateLimiterBuilder::new()
    }

    /// Builds a limiter with the given settings.
    ///
    /// Args:
    ///     config: The limits to apply.
    ///
    /// Returns:
    ///     A limiter with no keys.
    pub fn with_config(config: LoginRateLimiterConfig) -> Self {
        Self {
            config,
            states: KeyedState::new(),
        }
    }

    /// Forgets keys that aren't locked out and haven't failed for `max_idle`.
    ///
    /// Pruning also forgets the key's backoff history, so choose `max_idle`
    /// at least as long as the maximum lockout.
    ///
    /// Args:
    ///     max_idle: How long a key must have been quiet before it is removed.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     threads add keys at the same time.
    pub fn prune_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        self.states.retain(|state| {
            let recent = state
                .last_failure
                .is_some_and(|last| now.duration_since(last) < max_idle);
            state.active_lockout(now).is_some() || recent
        })
    }

    /// Number of keys currently tracked.
    ///
    /// Returns:
    ///     The key count.
    pub fn tracked_keys(&self) -> usize {
        self.states.len()
    }

    /// Whether the key may attempt a login now.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `true` unless the key is locked out.
    pub fn check_allowed(&self, key: &str) -> bool {
        self.check_status(key) == LoginStatus::Allowed
    }

    /// Whether the key may attempt a login now, and if not, for how long.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `Allowed`, or `LockedOut` with the whole seconds left.
    pub fn check_status(&self, key: &str) -> LoginStatus {
        let now = Instant::now();
        self.states
            .read(key, |state| state.active_lockout(now))
            .flatten()
            .map_or(LoginStatus::Allowed, |until| LoginStatus::LockedOut {
                retry_after: (until - now).as_secs().max(1),
            })
    }

    /// Records a failed login and locks the key out if it has reached the limit.
    ///
    /// A lockout that has already ended resets the failure count first, so
    /// each lockout is followed by a fresh set of attempts, while the backoff
    /// count keeps growing.
    ///
    /// Args:
    ///     key: The key that failed.
    pub fn record_failure(&self, key: &str) {
        let now = Instant::now();
        self.states.update(key, LoginState::default, |state| {
            if state.locked_until.is_some() && state.active_lockout(now).is_none() {
                state.locked_until = None;
                state.failures = 0;
            }
            state.failures += 1;
            state.last_failure = Some(now);
            if state.failures >= self.config.max_attempts {
                state.lockout_count += 1;
                let lockout = self.lockout_length(state.lockout_count);
                state.locked_until = Some(now + lockout);
                warn!(key = %pii(key), failures = state.failures, lockout_secs = lockout.as_secs(), "locked out after too many failed logins");
            }
        });
    }

    /// The length of the `count`th lockout, applying backoff and the cap.
    ///
    /// Args:
    ///     count: Which lockout this is, starting at 1.
    ///
    /// Returns:
    ///     How long the lockout lasts.
    fn lockout_length(&self, count: u32) -> Duration {
        if !self.config.exponential_backoff {
            return self.config.lockout_duration;
        }
        let factor = 2_u32.saturating_pow(count.saturating_sub(1));
        self.config
            .lockout_duration
            .saturating_mul(factor)
            .min(self.config.max_lockout_duration)
    }

    /// Records a successful login by forgetting the key, failures and backoff included.
    ///
    /// Args:
    ///     key: The key that succeeded.
    pub fn record_success(&self, key: &str) {
        if self
            .states
            .read(key, |state| state.failures > 0)
            .unwrap_or(false)
        {
            info!(key = %pii(key), "login succeeded, clearing failed attempts");
        }
        self.states.remove(key);
    }

    /// Failures that count towards the key's next lockout.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The failure count, 0 for an unknown key or once a lockout has ended.
    pub fn failure_count(&self, key: &str) -> u32 {
        let now = Instant::now();
        self.states
            .read(key, |state| state.effective_failures(now))
            .unwrap_or(0)
    }

    /// Failures the key can still make before it is locked out.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The remaining attempts.
    pub fn remaining_attempts(&self, key: &str) -> u32 {
        self.config
            .max_attempts
            .saturating_sub(self.failure_count(key))
    }

    /// Lifts a lockout and clears the failures, keeping the backoff count.
    ///
    /// Keeping the backoff count means a key that is unlocked by hand and
    /// keeps failing still gets longer lockouts. Unknown keys are ignored.
    ///
    /// Args:
    ///     key: The key to unlock.
    pub fn unlock(&self, key: &str) {
        let unlocked = self.states.update_existing(key, |state| {
            state.failures = 0;
            state.last_failure = None;
            state.locked_until = None;
        });
        if unlocked.is_some() {
            info!(key = %pii(key), "login lockout lifted by hand");
        }
    }

    /// Whether the key is locked out now.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     `true` during a lockout.
    pub fn is_locked(&self, key: &str) -> bool {
        !self.check_allowed(key)
    }

    /// How long the key's lockout has left.
    ///
    /// Args:
    ///     key: The key to check.
    ///
    /// Returns:
    ///     The time left, or `None` when the key isn't locked out.
    pub fn lockout_remaining(&self, key: &str) -> Option<Duration> {
        let now = Instant::now();
        self.states
            .read(key, |state| state.active_lockout(now))
            .flatten()
            .map(|until| until - now)
    }
}
