//! Sets up `tracing` output for the PopSub binary and tests, and holds the
//! switch for per-publish trace spans.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether each `publish` request gets its own trace span. On until the
/// binary turns it off, so tests and embedders see every request.
static PUBLISH_SPANS: AtomicBool = AtomicBool::new(true);

/// Turns per-publish trace spans on or off.
///
/// A span costs a formatted line of fields on every publish, which is the
/// busiest request a server handles. The binary turns them off unless traces
/// are being exported, where they are what a publish shows up as. Other
/// requests always get a span.
///
/// Args:
///     enabled: Whether to create a span for each publish.
pub fn set_publish_spans(enabled: bool) {
    PUBLISH_SPANS.store(enabled, Ordering::Relaxed);
}

/// Whether a `publish` request should get its own trace span.
///
/// Returns:
///     The value last given to `set_publish_spans`, true by default.
pub fn publish_spans() -> bool {
    PUBLISH_SPANS.load(Ordering::Relaxed)
}

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
