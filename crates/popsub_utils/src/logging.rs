//! Sets up `tracing` output for the PopSub binary and tests.

/// Installs a global `tracing_subscriber` formatter with a fixed maximum level.
///
/// `RUST_LOG` is not read; the level comes only from `default_level`. Log
/// lines leave out the target module. The subscriber is installed with
/// `try_init`, so calls after the first are ignored instead of panicking,
/// which lets tests and the binary both call it.
///
/// Args:
///     default_level: `error`, `warn` (or `warning`), `debug` or `trace`, in
///         any case. Anything else, including `info`, selects `INFO`.
pub fn init(default_level: &str) {
    let lvl = match default_level.to_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" | "warning" => tracing::Level::WARN,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };

    let _ = tracing_subscriber::fmt()
        .with_max_level(lvl)
        .with_target(false)
        .try_init();
}
