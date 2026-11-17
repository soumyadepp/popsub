//! Every metric PopSub records, as OpenTelemetry instruments.
//!
//! The crates that record them (transport and broker) only use the
//! OpenTelemetry API. Until the binary installs a meter provider (see
//! `[telemetry]` in the configuration) every instrument is a no-op that
//! costs an atomic load. The instruments are created on first use, from the
//! global meter provider at that moment, so the provider must be installed
//! before the server starts; instruments made earlier stay no-ops.
//!
//! Gauges that read the broker's state (connections, topics, subscriptions,
//! messages in flight) are registered by the binary, which owns the broker;
//! their names are here so the whole list is in one file.

use std::sync::OnceLock;

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};

/// The instrumentation scope every PopSub metric is recorded under.
pub const SCOPE: &str = "popsub";

/// Gauge: connections registered with the broker (after the handshake).
pub const CONNECTIONS: &str = "popsub.connections";
/// Gauge: topics with a writer or subscribers.
pub const TOPICS: &str = "popsub.topics";
/// Gauge: live subscriptions across all connections.
pub const SUBSCRIPTIONS: &str = "popsub.subscriptions";
/// Gauge: QoS 1 messages sent and not yet acknowledged.
pub const IN_FLIGHT: &str = "popsub.messages.in_flight";

/// The counters and histograms PopSub records.
#[derive(Debug)]
pub struct Metrics {
    /// Connections admitted after their handshakes.
    pub connections_opened: Counter<u64>,
    /// Connections turned away, by `reason`: `server_limit`, `address_limit`,
    /// `handshake`, `tls` or `proxy_header`.
    pub connections_refused: Counter<u64>,
    /// Connections ended, by `reason`: a close code's reason, `client` when
    /// the client closed it, or `dropped` when it went away without a close.
    pub connections_closed: Counter<u64>,
    /// Login attempts, by `outcome`: `success`, `invalid_credentials` or `locked_out`.
    pub logins: Counter<u64>,
    /// Error frames sent to clients, by `code`.
    pub errors: Counter<u64>,
    /// New records stored, by `qos`. Retried publishes that were already stored don't count.
    pub published: Counter<u64>,
    /// Records sent to subscribers, live or caught up from the log, retries excluded.
    pub delivered: Counter<u64>,
    /// QoS 1 records sent again because they weren't acknowledged in time.
    pub redelivered: Counter<u64>,
    /// QoS 1 acknowledgements accepted.
    pub acknowledged: Counter<u64>,
    /// Subscriptions paused after running out of retries.
    pub stalled: Counter<u64>,
    /// QoS 1 records given up on for one subscription and copied to the dead-letter topic.
    pub dead_lettered: Counter<u64>,
    /// How long a publish took in the topic writer: append, fan-out and trim, in seconds.
    pub publish_duration: Histogram<f64>,
    /// Storage operations that failed, by `operation`.
    pub storage_errors: Counter<u64>,
}

/// The instruments, created on first use.
static METRICS: OnceLock<Metrics> = OnceLock::new();

/// The instruments, created from the global meter provider on first use.
///
/// Returns:
///     The shared instruments.
pub fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| {
        let meter = global::meter(SCOPE);
        Metrics {
            connections_opened: meter
                .u64_counter("popsub.connections.opened")
                .with_unit("{connection}")
                .with_description("Connections admitted after their handshakes")
                .build(),
            connections_refused: meter
                .u64_counter("popsub.connections.refused")
                .with_unit("{connection}")
                .with_description("Connections turned away, by reason")
                .build(),
            connections_closed: meter
                .u64_counter("popsub.connections.closed")
                .with_unit("{connection}")
                .with_description("Connections ended, by close reason")
                .build(),
            logins: meter
                .u64_counter("popsub.logins")
                .with_unit("{attempt}")
                .with_description("Login attempts, by outcome")
                .build(),
            errors: meter
                .u64_counter("popsub.errors")
                .with_unit("{error}")
                .with_description("Error frames sent to clients, by code")
                .build(),
            published: meter
                .u64_counter("popsub.messages.published")
                .with_unit("{message}")
                .with_description("New records stored, by QoS")
                .build(),
            delivered: meter
                .u64_counter("popsub.messages.delivered")
                .with_unit("{message}")
                .with_description("Records sent to subscribers, retries excluded")
                .build(),
            redelivered: meter
                .u64_counter("popsub.messages.redelivered")
                .with_unit("{message}")
                .with_description("QoS 1 records sent again after an acknowledgement timeout")
                .build(),
            acknowledged: meter
                .u64_counter("popsub.messages.acknowledged")
                .with_unit("{message}")
                .with_description("QoS 1 acknowledgements accepted")
                .build(),
            stalled: meter
                .u64_counter("popsub.subscriptions.stalled")
                .with_unit("{subscription}")
                .with_description("Subscriptions paused after running out of retries")
                .build(),
            dead_lettered: meter
                .u64_counter("popsub.messages.dead_lettered")
                .with_unit("{message}")
                .with_description(
                    "QoS 1 records copied to the dead-letter topic after running out of retries",
                )
                .build(),
            publish_duration: meter
                .f64_histogram("popsub.publish.duration")
                .with_unit("s")
                .with_description("Time to append, fan out and trim one publish")
                .with_boundaries(vec![
                    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25,
                    0.5, 1.0,
                ])
                .build(),
            storage_errors: meter
                .u64_counter("popsub.storage.errors")
                .with_unit("{error}")
                .with_description("Storage operations that failed, by operation")
                .build(),
        }
    })
}

/// Builds a one-attribute set, the shape every attributed PopSub counter uses.
///
/// Args:
///     key: The attribute's name.
///     value: Its value.
///
/// Returns:
///     The attributes.
pub fn attr(key: &'static str, value: impl Into<opentelemetry::Value>) -> [KeyValue; 1] {
    [KeyValue::new(key, value)]
}
