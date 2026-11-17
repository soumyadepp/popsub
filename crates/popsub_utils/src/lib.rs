//! Shared building blocks for every PopSub crate: the `PopSubError` type, the
//! `Result` alias, logging setup, the OpenTelemetry metric instruments, and
//! masking personal data in logs (`pii`).
//!
//! This crate depends on no other PopSub crate and holds no domain logic.

pub mod error;
pub mod logging;
pub mod metrics;
pub mod pii;
pub mod topics;

#[cfg(test)]
mod tests {
    use super::logging;

    /// Checks that masked values are stable within the process, differ
    /// between values, hide the value, and print as is with masking off.
    #[test]
    fn pii_masks_and_unmasks() {
        use super::pii::{pii, set_masking};
        set_masking(true);
        let alice = pii("alice").to_string();
        assert!(alice.starts_with("masked:") && !alice.contains("alice"));
        assert_eq!(alice, pii(String::from("alice")).to_string());
        assert_eq!(format!("{:?}", pii("alice")), alice);
        assert_ne!(alice, pii("bob").to_string());
        set_masking(false);
        assert_eq!(pii("alice").to_string(), "alice");
        set_masking(true);
    }

    /// Checks that `logging::init` can be called repeatedly with different levels without panicking.
    #[test]
    fn logging_init_accepts_levels() {
        logging::init("info");
        logging::init("debug");
        logging::init("warn");
    }
}
