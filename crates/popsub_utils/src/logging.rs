/// Initialize tracing/logging for the application.
///
/// This uses a simple `with_max_level` configuration based on `default_level`.
pub fn init(default_level: &str) {
    let lvl = match default_level.to_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" | "warning" => tracing::Level::WARN,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };

    // Use try_init so tests and libraries can call this multiple times without panicking
    let _ = tracing_subscriber::fmt()
        .with_max_level(lvl)
        .with_target(false)
        .try_init();
}
