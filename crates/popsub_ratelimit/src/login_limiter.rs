//! Login Rate Limiter
//!
//! Specialized rate limiter for authentication with:
//! - Tracking failed login attempts
//! - Account lockout after max failures
//! - Automatic lockout expiry
//! - Success resets failure count

use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::{info, warn};

/// Status of a login attempt check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStatus {
    /// Login attempt is allowed.
    Allowed,
    /// Account is locked out.
    LockedOut {
        /// Seconds until lockout expires.
        retry_after: u64,
    },
}

/// State for a single user/IP.
#[derive(Default)]
struct LoginState {
    /// Number of consecutive failed attempts.
    failures: u32,
    /// Time of last failure (for lockout calculation).
    last_failure: Option<Instant>,
    /// Whether currently locked out.
    locked_until: Option<Instant>,
}

/// Configuration for login rate limiting.
#[derive(Debug, Clone)]
pub struct LoginRateLimiterConfig {
    /// Maximum failed attempts before lockout.
    pub max_attempts: u32,
    /// Duration of lockout.
    pub lockout_duration: Duration,
    /// Whether to use exponential backoff for repeated lockouts.
    pub exponential_backoff: bool,
    /// Maximum lockout duration (for exponential backoff).
    pub max_lockout_duration: Duration,
}

impl Default for LoginRateLimiterConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            lockout_duration: Duration::from_secs(300), // 5 minutes
            exponential_backoff: true,
            max_lockout_duration: Duration::from_secs(3600), // 1 hour
        }
    }
}

/// Builder for LoginRateLimiter.
pub struct LoginRateLimiterBuilder {
    config: LoginRateLimiterConfig,
}

impl LoginRateLimiterBuilder {
    pub fn new() -> Self {
        Self {
            config: LoginRateLimiterConfig::default(),
        }
    }

    /// Set maximum failed attempts before lockout.
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.config.max_attempts = attempts;
        self
    }

    /// Set lockout duration.
    pub fn lockout_duration(mut self, duration: Duration) -> Self {
        self.config.lockout_duration = duration;
        self
    }

    /// Enable/disable exponential backoff.
    pub fn exponential_backoff(mut self, enabled: bool) -> Self {
        self.config.exponential_backoff = enabled;
        self
    }

    /// Set maximum lockout duration (for exponential backoff).
    pub fn max_lockout_duration(mut self, duration: Duration) -> Self {
        self.config.max_lockout_duration = duration;
        self
    }

    /// Build the rate limiter.
    pub fn build(self) -> LoginRateLimiter {
        LoginRateLimiter {
            config: self.config,
            states: DashMap::new(),
        }
    }
}

impl Default for LoginRateLimiterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Login-specific rate limiter with lockout support.
///
/// # Example
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
/// // Before attempting login
/// if !limiter.check_allowed("username").await {
///     // Return "too many attempts" error
/// }
///
/// // After failed login
/// limiter.record_failure("username").await;
///
/// // After successful login
/// limiter.record_success("username").await;
/// ```
pub struct LoginRateLimiter {
    config: LoginRateLimiterConfig,
    states: DashMap<String, Mutex<LoginState>>,
}

impl LoginRateLimiter {
    /// Create a new login rate limiter with default config.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Create a builder for custom configuration.
    pub fn builder() -> LoginRateLimiterBuilder {
        LoginRateLimiterBuilder::new()
    }

    /// Create with specific config.
    pub fn with_config(config: LoginRateLimiterConfig) -> Self {
        Self {
            config,
            states: DashMap::new(),
        }
    }

    fn get_or_create_state(
        &self,
        key: &str,
    ) -> dashmap::mapref::one::Ref<'_, String, Mutex<LoginState>> {
        if !self.states.contains_key(key) {
            self.states
                .insert(key.to_string(), Mutex::new(LoginState::default()));
        }
        self.states.get(key).unwrap()
    }

    /// Check if a login attempt is allowed.
    ///
    /// Returns `true` if allowed, `false` if locked out.
    pub async fn check_allowed(&self, key: &str) -> bool {
        matches!(self.check_status(key).await, LoginStatus::Allowed)
    }

    /// Check status with details.
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

    /// Record a failed login attempt.
    ///
    /// Increments failure count and may trigger lockout.
    pub async fn record_failure(&self, key: &str) {
        let state_ref = self.get_or_create_state(key);
        let mut state = state_ref.lock().unwrap();
        let now = Instant::now();

        // Check if lockout has expired
        if let Some(locked_until) = state.locked_until
            && now >= locked_until
        {
            // Lockout expired, but keep failure count for exponential backoff
            state.locked_until = None;
        }

        state.failures += 1;
        state.last_failure = Some(now);

        if state.failures >= self.config.max_attempts {
            let lockout_duration = if self.config.exponential_backoff {
                // Exponential backoff: double duration for each lockout
                let multiplier = state.failures / self.config.max_attempts;
                let base_millis = self.config.lockout_duration.as_millis() as u64;
                let exponential_millis =
                    base_millis.saturating_mul(2_u64.saturating_pow(multiplier.saturating_sub(1)));
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

    /// Record a successful login.
    ///
    /// Resets failure count and clears lockout.
    pub async fn record_success(&self, key: &str) {
        let state_ref = self.get_or_create_state(key);
        let mut state = state_ref.lock().unwrap();

        if state.failures > 0 {
            info!(key = %key, previous_failures = state.failures, "Login successful, resetting failure count");
        }

        state.failures = 0;
        state.last_failure = None;
        state.locked_until = None;
    }

    /// Get the number of failed attempts for a key.
    pub async fn failure_count(&self, key: &str) -> u32 {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();
        state.failures
    }

    /// Get remaining attempts before lockout.
    pub async fn remaining_attempts(&self, key: &str) -> u32 {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();
        self.config.max_attempts.saturating_sub(state.failures)
    }

    /// Manually unlock an account (admin action).
    pub async fn unlock(&self, key: &str) {
        let state_ref = self.get_or_create_state(key);
        let mut state = state_ref.lock().unwrap();
        state.failures = 0;
        state.last_failure = None;
        state.locked_until = None;
        info!(key = %key, "Account manually unlocked");
    }

    /// Check if an account is currently locked.
    pub async fn is_locked(&self, key: &str) -> bool {
        let state_ref = self.get_or_create_state(key);
        let state = state_ref.lock().unwrap();

        if let Some(locked_until) = state.locked_until {
            Instant::now() < locked_until
        } else {
            false
        }
    }

    /// Get time until lockout expires (if locked).
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
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_separate_keys() {
        let limiter = LoginRateLimiter::builder().max_attempts(2).build();

        limiter.record_failure("user1").await;
        limiter.record_failure("user1").await;

        assert!(!limiter.check_allowed("user1").await);
        assert!(limiter.check_allowed("user2").await);
    }

    #[tokio::test]
    async fn test_remaining_attempts() {
        let limiter = LoginRateLimiter::builder().max_attempts(5).build();

        assert_eq!(limiter.remaining_attempts("user").await, 5);
        limiter.record_failure("user").await;
        assert_eq!(limiter.remaining_attempts("user").await, 4);
    }

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

    #[tokio::test]
    async fn test_exponential_backoff() {
        let limiter = LoginRateLimiter::builder()
            .max_attempts(1)
            .lockout_duration(Duration::from_millis(10))
            .exponential_backoff(true)
            .max_lockout_duration(Duration::from_secs(100))
            .build();

        // First lockout
        limiter.record_failure("user").await;
        assert!(!limiter.check_allowed("user").await);

        // Wait for first lockout to expire
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(limiter.check_allowed("user").await);

        // Second failure - should have longer lockout
        limiter.record_failure("user").await;
        let remaining = limiter.lockout_remaining("user").await;
        assert!(remaining.is_some());
        // Second lockout should be ~20ms (2x first)
        assert!(remaining.unwrap() > Duration::from_millis(10));
    }

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
