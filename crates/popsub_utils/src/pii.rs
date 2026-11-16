//! Masking personal data in logs and telemetry.
//!
//! Log calls wrap identifying values (usernames, client addresses, session
//! ids) in `pii`. While masking is on, the default, such a value prints as
//! `masked:` and eight hex digits: a keyed hash (SipHash) under a key drawn
//! at random when the process starts. The same value always prints the same
//! within one process, so one user's events can still be followed, but the
//! name can't be read back without the key, and the key is never written
//! out. Turning masking off (`telemetry.mask_pii = false`) prints values as
//! they are, for debugging on a machine whose logs stay private.
//!
//! The hash is for keeping names out of log stores, not encryption: a short
//! hash of a short value can collide, and anyone who can run code in the
//! process can recompute it.

use std::fmt;
use std::hash::BuildHasher;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `Pii` values are masked.
static MASKING: AtomicBool = AtomicBool::new(true);

/// The key masked values are hashed under, drawn once per process.
static KEY: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();

/// Turns masking on or off for the whole process.
///
/// Args:
///     on: Whether `Pii` values print masked.
pub fn set_masking(on: bool) {
    MASKING.store(on, Ordering::SeqCst);
}

/// Whether masking is on.
///
/// Returns:
///     True while `Pii` values print masked.
pub fn masking() -> bool {
    MASKING.load(Ordering::SeqCst)
}

/// A value that identifies a person, printed masked while masking is on.
#[derive(Clone, Copy)]
pub struct Pii<T>(T);

/// Marks a value as identifying a person, for a log field or message.
///
/// Example:
///
/// ```
/// use popsub_utils::pii::{pii, set_masking};
///
/// set_masking(true);
/// let shown = pii("alice").to_string();
/// assert!(shown.starts_with("masked:"));
/// assert_eq!(shown, pii("alice").to_string());
/// ```
///
/// Args:
///     value: The value, such as a username or an address.
///
/// Returns:
///     A wrapper that prints the value masked or as is.
pub fn pii<T: fmt::Display>(value: T) -> Pii<T> {
    Pii(value)
}

impl<T: fmt::Display> fmt::Display for Pii<T> {
    /// Prints `masked:` and the value's keyed hash, or the value itself
    /// when masking is off.
    ///
    /// Args:
    ///     f: The formatter.
    ///
    /// Raises:
    ///     fmt::Error: When the formatter fails.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !masking() {
            return self.0.fmt(f);
        }
        let hash = KEY
            .get_or_init(std::collections::hash_map::RandomState::new)
            .hash_one(self.0.to_string());
        write!(f, "masked:{:08x}", hash >> 32)
    }
}

impl<T: fmt::Display> fmt::Debug for Pii<T> {
    /// Prints the same as `Display`, so `?field` masks too.
    ///
    /// Args:
    ///     f: The formatter.
    ///
    /// Raises:
    ///     fmt::Error: When the formatter fails.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
