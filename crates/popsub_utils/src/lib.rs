//! Shared building blocks for every PopSub crate: the `PopSubError` type, the
//! `Result` alias and logging setup.
//!
//! This crate depends on no other PopSub crate and holds no domain logic.

pub mod error;
pub mod logging;

#[cfg(test)]
mod tests {
    use super::logging;

    /// Checks that `logging::init` can be called repeatedly with different levels without panicking.
    #[test]
    fn logging_init_accepts_levels() {
        logging::init("info");
        logging::init("debug");
        logging::init("warn");
    }
}
