//! Health endpoints for load balancers and orchestrators, served over plain
//! HTTP on their own address, apart from the WebSocket listener.
//!
//! - `GET /healthz` (liveness): 200 whenever the process can answer.
//! - `GET /readyz` (readiness): 200 when the server should get traffic, 503
//!   while it is shutting down or while topic appends are failing.
//!
//! Both answer with a small JSON body and accept `HEAD`. Appends count as
//! failing for `STORAGE_FAILURE_WINDOW` after the last failure unless one
//! succeeds since; see `popsub_broker::StorageHealth` for why it is a window.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use popsub_broker::SharedBroker;
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

/// How long a failed append keeps the server not ready, unless one succeeds.
pub const STORAGE_FAILURE_WINDOW: Duration = Duration::from_secs(30);

/// How long a client has to send a request's headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one health connection may stay open.
const CONNECTION_LIMIT: Duration = Duration::from_secs(30);

/// How long the accept loop waits after a failed `accept`.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// What the health endpoints report on. Cloning shares the state.
#[derive(Clone, Debug)]
pub struct Health {
    /// The broker, for whether storage is failing.
    broker: SharedBroker,
    /// Whether the server is shutting down and should get no new traffic.
    draining: Arc<AtomicBool>,
}

impl Health {
    /// Builds the health state for a server.
    ///
    /// Args:
    ///     broker: The server's broker.
    ///
    /// Returns:
    ///     The state, ready.
    pub fn new(broker: SharedBroker) -> Self {
        Self {
            broker,
            draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Marks the server as shutting down, so `/readyz` answers 503 from now on.
    pub fn drain(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    /// Whether the server should get traffic.
    ///
    /// Returns:
    ///     `Ok` when ready.
    ///
    /// Raises:
    ///     String: Why it isn't: shutting down, or appends failing.
    pub fn readiness(&self) -> Result<(), String> {
        if self.draining.load(Ordering::SeqCst) {
            return Err("shutting down".to_string());
        }
        if let Some(ago) = self.broker.storage_failing(STORAGE_FAILURE_WINDOW) {
            return Err(format!(
                "topic appends are failing (last failure {}s ago)",
                ago.as_secs()
            ));
        }
        Ok(())
    }

    /// Serves the health endpoints on a bound listener until the future is dropped.
    ///
    /// Each connection must send its headers within `HEADER_TIMEOUT` and is
    /// closed after `CONNECTION_LIMIT`. A failed `accept` is logged and
    /// retried after `ACCEPT_BACKOFF`.
    ///
    /// Args:
    ///     listener: The bound listener.
    pub async fn serve(self, listener: TcpListener) {
        if let Ok(addr) = listener.local_addr() {
            info!("health endpoints on http://{addr}/healthz and /readyz");
        }
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    warn!(%e, "accepting a health connection failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                    continue;
                }
            };
            let health = self.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let response = health.respond(&request);
                    async move { Ok::<_, Infallible>(response) }
                });
                let connection = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_TIMEOUT)
                    .serve_connection(TokioIo::new(stream), service);
                match tokio::time::timeout(CONNECTION_LIMIT, connection).await {
                    Ok(Err(e)) => debug!(%e, "health connection failed"),
                    Err(_) => debug!("closed a health connection open too long"),
                    Ok(Ok(())) => {}
                }
            });
        }
    }

    /// Answers one request.
    ///
    /// Args:
    ///     request: The request.
    ///
    /// Returns:
    ///     The response: the endpoint's status and JSON body, 405 for
    ///     methods other than `GET` and `HEAD`, 404 for other paths.
    fn respond<B>(&self, request: &Request<B>) -> Response<Full<Bytes>> {
        if request.method() != Method::GET && request.method() != Method::HEAD {
            let mut response = json(
                StatusCode::METHOD_NOT_ALLOWED,
                serde_json::json!({"status": "method_not_allowed"}),
            );
            response
                .headers_mut()
                .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
            return response;
        }
        match request.uri().path() {
            "/healthz" => json(StatusCode::OK, serde_json::json!({"status": "ok"})),
            "/readyz" => match self.readiness() {
                Ok(()) => json(StatusCode::OK, serde_json::json!({"status": "ready"})),
                Err(reason) => json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    serde_json::json!({"status": "not_ready", "reason": reason}),
                ),
            },
            _ => json(
                StatusCode::NOT_FOUND,
                serde_json::json!({"status": "not_found"}),
            ),
        }
    }
}

/// Builds a JSON response that caches nowhere.
///
/// Args:
///     status: The status code.
///     body: The body.
///
/// Returns:
///     The response.
fn json(status: StatusCode, body: serde_json::Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Tests for the endpoints, over real HTTP connections.
#[cfg(test)]
mod tests {
    use super::*;
    use popsub_broker::{Broker, LogSettings};
    use popsub_config::settings::BrokerSettings;
    use popsub_storage::{Durability, FaultyStorage, MemoryStorage, NewRecord, Retention};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Starts health endpoints for a broker whose storage can be broken.
    ///
    /// Returns:
    ///     The address, the health state, the broker and the storage switch.
    async fn start() -> (std::net::SocketAddr, Health, SharedBroker, FaultyStorage) {
        let storage = FaultyStorage::new(Arc::new(MemoryStorage::new()));
        let broker = SharedBroker::new(
            Broker::new(BrokerSettings::default()),
            Arc::new(storage.clone()),
            LogSettings {
                durability: Durability::None,
                retention: Retention::default(),
                session_expiry: Duration::from_secs(60),
            },
        )
        .expect("broker");
        let health = Health::new(broker.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(health.clone().serve(listener));
        (addr, health, broker, storage)
    }

    /// Sends one HTTP request and reads the whole response.
    ///
    /// Args:
    ///     addr: The health address.
    ///     method: The method.
    ///     path: The path.
    ///
    /// Returns:
    ///     The status code and body.
    async fn get(addr: std::net::SocketAddr, method: &str, path: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_string(&mut response))
            .await
            .expect("a response in time")
            .expect("read");
        let status = response
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("a status line");
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }

    /// A record to publish.
    ///
    /// Args:
    ///     id: Its message id.
    ///
    /// Returns:
    ///     The record.
    fn record(id: &str) -> NewRecord {
        NewRecord {
            message_id: id.to_string(),
            publisher: "alice".to_string(),
            payload: "x".to_string(),
            qos: 1,
            published_at_ms: 1,
        }
    }

    /// Checks that both endpoints answer 200 with JSON, `HEAD` works, and
    /// other paths and methods get 404 and 405.
    #[tokio::test]
    async fn endpoints_answer_when_healthy() {
        let (addr, _, _, _) = start().await;
        assert_eq!(
            get(addr, "GET", "/healthz").await,
            (200, r#"{"status":"ok"}"#.to_string())
        );
        assert_eq!(
            get(addr, "GET", "/readyz").await,
            (200, r#"{"status":"ready"}"#.to_string())
        );
        assert_eq!(get(addr, "HEAD", "/readyz").await, (200, String::new()));
        assert_eq!(get(addr, "GET", "/metrics").await.0, 404);
        assert_eq!(get(addr, "POST", "/healthz").await.0, 405);
    }

    /// Checks that failing appends make `/readyz` answer 503 with the reason
    /// while `/healthz` stays 200, and that a successful append clears it.
    #[tokio::test]
    async fn failing_storage_makes_the_server_not_ready() {
        let (addr, _, broker, storage) = start().await;
        storage.set_failing(true);
        assert!(broker.publish("orders", record("lost")).await.is_err());
        let (status, body) = get(addr, "GET", "/readyz").await;
        assert_eq!(status, 503);
        assert!(body.contains("appends are failing"), "{body}");
        assert_eq!(get(addr, "GET", "/healthz").await.0, 200);
        storage.set_failing(false);
        broker
            .publish("orders", record("kept"))
            .await
            .expect("publish");
        assert_eq!(get(addr, "GET", "/readyz").await.0, 200);
    }

    /// Checks that draining turns `/readyz` to 503 and leaves `/healthz` at 200.
    #[tokio::test]
    async fn draining_makes_the_server_not_ready() {
        let (addr, health, _, _) = start().await;
        health.drain();
        let (status, body) = get(addr, "GET", "/readyz").await;
        assert_eq!(status, 503);
        assert!(body.contains("shutting down"), "{body}");
        assert_eq!(get(addr, "GET", "/healthz").await.0, 200);
    }
}
