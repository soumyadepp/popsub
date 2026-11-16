//! `KeyedState`, the per-key store every limiter in this crate is built on.
//!
//! Each limiter needs the same things: create a key's state on first use,
//! read it without creating it, change it under a lock, forget idle keys, and
//! never panic because another thread panicked. Keeping that here means the
//! limiters only describe their own arithmetic.

use std::sync::{Mutex, MutexGuard, PoisonError};

use dashmap::DashMap;

/// A concurrent map from string keys to per-key state, each behind its own mutex.
pub(crate) struct KeyedState<T> {
    /// The state for every key seen and not yet removed.
    entries: DashMap<String, Mutex<T>>,
}

impl<T> KeyedState<T> {
    /// Creates an empty store.
    ///
    /// Returns:
    ///     A store with no keys.
    pub(crate) fn new() -> Self {
        Self {
            entries: DashMap::new(),
        }
    }

    /// Runs `f` on a key's state, creating it with `init` if the key is new.
    ///
    /// The entry is created and used through the same reference, so a
    /// concurrent `remove` can't make the lookup fail.
    ///
    /// Args:
    ///     key: The key to update.
    ///     init: Builds the state for a key seen for the first time.
    ///     f: The change to make, run while holding the key's lock.
    ///
    /// Returns:
    ///     Whatever `f` returns.
    pub(crate) fn update<R>(
        &self,
        key: &str,
        init: impl FnOnce() -> T,
        f: impl FnOnce(&mut T) -> R,
    ) -> R {
        let entry = self
            .entries
            .entry(key.to_string())
            .or_insert_with(|| Mutex::new(init()))
            .downgrade();
        let mut state = lock(&entry);
        f(&mut state)
    }

    /// Runs `f` on a key's state only if the key already exists.
    ///
    /// Args:
    ///     key: The key to update.
    ///     f: The change to make, run while holding the key's lock.
    ///
    /// Returns:
    ///     What `f` returned, or `None` when the key is unknown.
    pub(crate) fn update_existing<R>(&self, key: &str, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let entry = self.entries.get(key)?;
        let mut state = lock(&entry);
        Some(f(&mut state))
    }

    /// Reads a key's state without creating it.
    ///
    /// Args:
    ///     key: The key to read.
    ///     f: Computes the answer from the state.
    ///
    /// Returns:
    ///     What `f` returned, or `None` when the key is unknown.
    pub(crate) fn read<R>(&self, key: &str, f: impl FnOnce(&T) -> R) -> Option<R> {
        let entry = self.entries.get(key)?;
        let state = lock(&entry);
        Some(f(&state))
    }

    /// Forgets a key.
    ///
    /// Args:
    ///     key: The key to remove. Unknown keys are ignored.
    pub(crate) fn remove(&self, key: &str) {
        self.entries.remove(key);
    }

    /// Keeps only the keys whose state passes `keep`.
    ///
    /// Args:
    ///     keep: Decides, for each key's state, whether to keep it.
    ///
    /// Returns:
    ///     How many fewer keys are tracked afterwards. Approximate when other
    ///     threads add keys at the same time.
    pub(crate) fn retain(&self, mut keep: impl FnMut(&mut T) -> bool) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, state| keep(state.get_mut().unwrap_or_else(PoisonError::into_inner)));
        before.saturating_sub(self.entries.len())
    }

    /// Number of keys currently tracked.
    ///
    /// Returns:
    ///     The key count.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Locks one key's state, recovering it if a previous holder panicked.
///
/// A limiter's state is a few counters and timestamps that are valid after
/// any partial update, so continuing is safer than spreading the panic.
///
/// Args:
///     state: The key's mutex.
///
/// Returns:
///     A guard over the state.
fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}
