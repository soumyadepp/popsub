//! Checks that a real server records the OpenTelemetry metrics it promises.
//!
//! This is its own test binary because the instruments bind to the global
//! meter provider the first time they are used: the provider has to be
//! installed before anything in the process records a metric.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::Settings;
use popsub_storage::{Durability, MemoryStorage};
use popsub_transport::Server;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// A client connection in this test.
type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// Reads frames until one of the given type arrives.
///
/// Args:
///     ws: The connection.
///     kind: The `type` to wait for.
///
/// Returns:
///     That frame.
async fn until(ws: &mut Socket, kind: &str) -> serde_json::Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("a frame")
            .expect("open")
            .expect("read");
        if let WsMessage::Text(text) = frame {
            let frame: serde_json::Value = serde_json::from_str(&text).expect("JSON");
            if frame["type"] == kind || frame["type"] == "error" {
                return frame;
            }
        }
    }
}

/// Sends a frame.
///
/// Args:
///     ws: The connection.
///     frame: The frame.
async fn send(ws: &mut Socket, frame: serde_json::Value) {
    ws.send(WsMessage::Text(frame.to_string().into()))
        .await
        .expect("send");
}

/// Opens a connection.
///
/// Args:
///     addr: The server's address.
///
/// Returns:
///     The connection.
async fn connect(addr: std::net::SocketAddr) -> Socket {
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .expect("handshake")
        .0
}

/// Adds up the `u64` sums exported for each metric and attribute set.
///
/// Args:
///     exporter: The in-memory exporter.
///
/// Returns:
///     Totals keyed by `name` or `name{key=value}`, plus histogram counts
///     keyed by `name.count`.
fn totals(exporter: &InMemoryMetricExporter) -> HashMap<String, u64> {
    let mut totals = HashMap::new();
    for resource in exporter.get_finished_metrics().expect("metrics") {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                match metric.data() {
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                        for point in sum.data_points() {
                            let labels: Vec<String> = point
                                .attributes()
                                .map(|kv| format!("{}={}", kv.key, kv.value))
                                .collect();
                            let key = if labels.is_empty() {
                                metric.name().to_string()
                            } else {
                                format!("{}{{{}}}", metric.name(), labels.join(","))
                            };
                            totals.insert(key, point.value());
                        }
                    }
                    AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                        let count = histogram.data_points().map(|p| p.count()).sum();
                        totals.insert(format!("{}.count", metric.name()), count);
                    }
                    _ => {}
                }
            }
        }
    }
    totals
}

/// Checks the counters and the publish histogram after a login, a failed
/// login, a QoS 1 publish, its delivery and its acknowledgement.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_records_its_metrics() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());

    let settings = Settings::default();
    let broker = SharedBroker::new(
        Broker::new(settings.broker.clone()),
        Arc::new(MemoryStorage::new()),
        LogSettings {
            durability: Durability::None,
            retention: settings.storage.retention.to_retention(),
            session_expiry: Duration::from_secs(60),
        },
    )
    .expect("broker");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let server = Server::from_settings(broker, &settings.server).await;
    tokio::spawn(server.serve(listener));

    let mut wrong = connect(addr).await;
    send(
        &mut wrong,
        serde_json::json!({"type": "login", "username": "admin", "password": "not-it"}),
    )
    .await;
    assert_eq!(
        until(&mut wrong, "logged_in").await["code"],
        "invalid_credentials"
    );

    let mut ws = connect(addr).await;
    send(
        &mut ws,
        serde_json::json!({"type": "login", "username": "admin", "password": "password"}),
    )
    .await;
    let token = until(&mut ws, "logged_in").await["token"].clone();
    send(&mut ws, serde_json::json!({"type": "auth", "token": token})).await;
    until(&mut ws, "authenticated").await;
    send(
        &mut ws,
        serde_json::json!({"type": "subscribe", "topic": "orders"}),
    )
    .await;
    until(&mut ws, "subscribed").await;
    send(
        &mut ws,
        serde_json::json!({"type": "publish", "topic": "orders", "payload": "x", "qos": 1}),
    )
    .await;
    let delivery = until(&mut ws, "message").await;
    send(
        &mut ws,
        serde_json::json!({"type": "ack", "topic": "orders", "seq": delivery["seq"]}),
    )
    .await;
    send(
        &mut ws,
        serde_json::json!({"type": "unsubscribe", "topic": "orders"}),
    )
    .await;
    until(&mut ws, "unsubscribed").await;

    provider.force_flush().expect("flush");
    let totals = totals(&exporter);
    let get = |key: &str| totals.get(key).copied().unwrap_or(0);
    assert_eq!(get("popsub.logins{outcome=success}"), 1, "{totals:?}");
    assert_eq!(
        get("popsub.logins{outcome=invalid_credentials}"),
        1,
        "{totals:?}"
    );
    assert_eq!(
        get("popsub.errors{code=invalid_credentials}"),
        1,
        "{totals:?}"
    );
    assert_eq!(get("popsub.connections.opened"), 2, "{totals:?}");
    assert_eq!(get("popsub.messages.published{qos=1}"), 1, "{totals:?}");
    assert_eq!(get("popsub.messages.delivered"), 1, "{totals:?}");
    assert_eq!(get("popsub.messages.acknowledged"), 1, "{totals:?}");
    assert_eq!(get("popsub.publish.duration.count"), 1, "{totals:?}");
}
