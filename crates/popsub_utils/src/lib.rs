pub mod error;
pub mod logging;

#[cfg(test)]
mod tests {
    use super::logging;

    #[test]
    fn logging_init_accepts_levels() {
        // Should not panic
        logging::init("info");
        logging::init("debug");
        logging::init("warn");
    }
}
