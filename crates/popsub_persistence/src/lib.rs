//! Stores recent messages per topic in an embedded `sled` database so the
//! broker can replay history to new subscribers.
//!
//! The storage itself is in `sled_store`; callers only see `Persistence` and
//! `StoredMessage`, which keeps the backend replaceable.

pub mod sled_store;
#[cfg(test)]
mod tests;
pub use sled_store::Persistence;
