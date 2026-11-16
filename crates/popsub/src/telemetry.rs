//! Logging and OpenTelemetry for the server.
//!
//! Log lines always go to standard output. With `[telemetry] enabled`, the
//! server also exports over OTLP (HTTP and protobuf):
//!
//! - metrics: the instruments in `popsub_utils::metrics`, plus gauges read
//!   from the broker (`register_gauges`), every 60 seconds by default
//!   (`OTEL_METRIC_EXPORT_INTERVAL` in milliseconds changes it);
//! - traces: one span per client request (acks excepted), sampled by
//!   `sample_ratio`;
//! - logs: the same events as the log lines, linked to the request's trace.
//!
//! The destination and the resource come from the standard variables
//! (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`,
//! `OTEL_RESOURCE_ATTRIBUTES`). The service name defaults to `popsub`.
//! Only plain `http://` endpoints are supported: send to an OpenTelemetry
//! Collector, which can forward over TLS. Exports that fail are dropped and
//! never slow the server down.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{MeterProvider as _, ObservableGauge};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use popsub_broker::SharedBroker;
use popsub_config::TelemetrySettings;
use popsub_utils::metrics;
use tracing::Level;
use tracing_subscriber::filter::{LevelFilter, filter_fn};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, fmt};

/// Crates whose own log events are kept out of the OTLP log export, since
/// exporting them would log the exporter's own requests in a loop.
const NOISY_TARGETS: [&str; 5] = ["opentelemetry", "hyper", "reqwest", "h2", "tower"];

/// The installed OpenTelemetry providers, flushed and stopped by `shutdown`.
#[derive(Default)]
pub struct Telemetry {
    /// Exports traces, when enabled.
    tracer: Option<SdkTracerProvider>,
    /// Exports metrics, when enabled.
    meter: Option<SdkMeterProvider>,
    /// Exports logs, when enabled.
    logger: Option<SdkLoggerProvider>,
    /// The broker gauges, kept so their callbacks stay registered.
    gauges: Vec<ObservableGauge<u64>>,
}

impl Telemetry {
    /// Starts logging and, when enabled, the OpenTelemetry exporters, and
    /// installs the meter provider globally so `popsub_utils::metrics` records into it.
    ///
    /// Must run before anything records a metric: instruments created
    /// before the provider is installed stay no-ops.
    ///
    /// Args:
    ///     level: The most detailed level logged.
    ///     settings: The `[telemetry]` section.
    ///
    /// Returns:
    ///     The providers, to shut down before exiting.
    ///
    /// Raises:
    ///     String: When an exporter can't be built, or `sample_ratio` isn't between 0 and 1.
    pub fn init(level: Level, settings: &TelemetrySettings) -> Result<Self, String> {
        let console = fmt::layer()
            .with_target(false)
            .with_filter(LevelFilter::from_level(level));
        if !settings.enabled {
            let _ = tracing_subscriber::registry().with(console).try_init();
            return Ok(Self::default());
        }
        if !(0.0..=1.0).contains(&settings.sample_ratio) {
            return Err(format!(
                "telemetry.sample_ratio must be between 0 and 1, not {}",
                settings.sample_ratio
            ));
        }
        let resource = resource();
        let failed = |signal: &str, e: &dyn std::fmt::Display| {
            format!("can't start the OTLP {signal} exporter: {e}")
        };

        let spans = SpanExporter::builder()
            .with_http()
            .build()
            .map_err(|e| failed("trace", &e))?;
        let tracer = SdkTracerProvider::builder()
            .with_batch_exporter(spans)
            .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                settings.sample_ratio,
            ))))
            .with_resource(resource.clone())
            .build();
        opentelemetry::global::set_tracer_provider(tracer.clone());

        let metric_exporter = MetricExporter::builder()
            .with_http()
            .build()
            .map_err(|e| failed("metric", &e))?;
        let meter = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource.clone())
            .build();
        opentelemetry::global::set_meter_provider(meter.clone());

        let log_exporter = LogExporter::builder()
            .with_http()
            .build()
            .map_err(|e| failed("log", &e))?;
        let logger = SdkLoggerProvider::builder()
            .with_batch_exporter(log_exporter)
            .with_resource(resource)
            .build();

        let traces = tracing_opentelemetry::layer()
            .with_tracer(tracer.tracer(metrics::SCOPE))
            .with_filter(LevelFilter::from_level(level));
        let logs = OpenTelemetryTracingBridge::new(&logger)
            .with_filter(LevelFilter::from_level(level))
            .with_filter(filter_fn(|meta| {
                !NOISY_TARGETS
                    .iter()
                    .any(|noisy| meta.target().starts_with(noisy))
            }));
        let _ = tracing_subscriber::registry()
            .with(console)
            .with(traces)
            .with(logs)
            .try_init();
        Ok(Self {
            tracer: Some(tracer),
            meter: Some(meter),
            logger: Some(logger),
            gauges: Vec::new(),
        })
    }

    /// Registers the gauges read from the broker: connections, topics,
    /// subscriptions and messages in flight. Each export reads one
    /// `SharedBroker::stats` snapshot per gauge. Does nothing when
    /// telemetry is off.
    ///
    /// Args:
    ///     broker: The server's broker.
    pub fn register_gauges(&mut self, broker: &SharedBroker) {
        let Some(provider) = &self.meter else {
            return;
        };
        let meter = provider.meter(metrics::SCOPE);
        let gauge = |name: &'static str,
                     unit: &'static str,
                     read: fn(&popsub_broker::BrokerStats) -> u64| {
            let broker = broker.clone();
            meter
                .u64_observable_gauge(name)
                .with_unit(unit)
                .with_callback(move |observer| observer.observe(read(&broker.stats()), &[]))
                .build()
        };
        self.gauges = vec![
            gauge(metrics::CONNECTIONS, "{connection}", |s| s.connections),
            gauge(metrics::TOPICS, "{topic}", |s| s.topics),
            gauge(metrics::SUBSCRIPTIONS, "{subscription}", |s| {
                s.subscriptions
            }),
            gauge(metrics::IN_FLIGHT, "{message}", |s| s.in_flight),
        ];
    }

    /// Flushes what is still buffered and stops the exporters. Blocks for up
    /// to a few seconds when a collector is slow or unreachable.
    pub fn shutdown(self) {
        if let Some(tracer) = self.tracer {
            let _ = tracer.shutdown();
        }
        if let Some(meter) = self.meter {
            let _ = meter.shutdown();
        }
        if let Some(logger) = self.logger {
            let _ = logger.shutdown();
        }
    }
}

/// Describes this process to the telemetry backend: `service.name` from
/// `OTEL_SERVICE_NAME` or `popsub`, `service.version`, and anything in
/// `OTEL_RESOURCE_ATTRIBUTES`.
///
/// Returns:
///     The resource.
fn resource() -> Resource {
    let mut builder = Resource::builder()
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")));
    if std::env::var_os("OTEL_SERVICE_NAME").is_none() {
        builder = builder.with_service_name("popsub");
    }
    builder.build()
}

/// Unit tests for the telemetry settings checks.
#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that a sample ratio outside 0 to 1 is refused before any
    /// exporter starts, and that disabled telemetry starts nothing.
    #[test]
    fn bad_sample_ratios_are_refused() {
        for ratio in [-0.1, 1.5, f64::NAN] {
            let settings = TelemetrySettings {
                enabled: true,
                sample_ratio: ratio,
                ..TelemetrySettings::default()
            };
            assert!(Telemetry::init(Level::INFO, &settings).is_err(), "{ratio}");
        }
        let off = Telemetry::init(Level::INFO, &TelemetrySettings::default()).expect("off");
        assert!(off.tracer.is_none() && off.meter.is_none() && off.logger.is_none());
    }
}
