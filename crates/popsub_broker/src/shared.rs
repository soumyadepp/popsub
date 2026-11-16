//! `SharedBroker`, the one handle every task uses to reach the broker.
//!
//! The broker is a plain synchronous struct. `SharedBroker` owns the mutex
//! around it and only exposes `with`, which runs a closure while holding the
//! lock. A closure can't `.await`, so no caller can hold the lock across an
//! await point, and the poisoning policy lives in one place instead of at
//! every call site.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tracing::{error, info};

use crate::engine::Broker;

/// How often `run_retry_loop` looks for overdue QoS 1 messages.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// A cloneable handle to the broker shared by every connection and background task.
///
/// Cloning is cheap and every clone refers to the same broker.
#[derive(Clone, Debug)]
pub struct SharedBroker {
    /// The broker behind the only lock that guards it.
    inner: Arc<Mutex<Broker>>,
}

impl SharedBroker {
    /// Wraps a broker so it can be shared between tasks.
    ///
    /// Args:
    ///     broker: The broker to share.
    ///
    /// Returns:
    ///     A handle to the broker.
    pub fn new(broker: Broker) -> Self {
        Self {
            inner: Arc::new(Mutex::new(broker)),
        }
    }

    /// Runs `f` with exclusive access to the broker and returns its result.
    ///
    /// If a previous holder panicked, the lock is poisoned. Rather than taking
    /// the whole server down with it, the error is logged and the broker is
    /// used as it was left: every broker operation leaves its maps usable,
    /// even when interrupted.
    ///
    /// Args:
    ///     f: The work to do while holding the lock. It must not block.
    ///
    /// Returns:
    ///     Whatever `f` returns.
    pub fn with<R>(&self, f: impl FnOnce(&mut Broker) -> R) -> R {
        let mut guard = self.lock();
        f(&mut guard)
    }

    /// Takes the lock, recovering from poisoning as `with` describes.
    ///
    /// Returns:
    ///     A guard giving exclusive access to the broker.
    fn lock(&self) -> MutexGuard<'_, Broker> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            error!("broker lock was poisoned by a panicking task, continuing with its state");
            poisoned.into_inner()
        })
    }

    /// Resends overdue QoS 1 messages every `RETRY_INTERVAL`, forever.
    ///
    /// Spawn this as a background task. Each tick runs one
    /// `Broker::retry_overdue` pass under the lock.
    pub async fn run_retry_loop(self) {
        let mut ticker = tokio::time::interval(RETRY_INTERVAL);
        loop {
            ticker.tick().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let outcome = self.with(|broker| broker.retry_overdue(now_ms));
            if outcome.resent > 0 || outcome.dropped > 0 {
                info!(
                    resent = outcome.resent,
                    dropped = outcome.dropped,
                    "retried QoS 1 messages"
                );
            }
        }
    }
}
