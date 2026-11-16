//! The PopSub server a scenario runs against, which the scenario can pause,
//! restart and publish to directly.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use popsub_auth::{InMemoryUserStore, UserStore};
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::Settings;
use popsub_protocol::CloseCode;
use popsub_storage::{Durability, FileStorage, MemoryStorage, NewRecord, Storage};
use popsub_transport::Server;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Where a scenario server keeps its topic logs and sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// In memory; gone when the server is restarted.
    Memory,
    /// In a temporary directory; survives `restart`.
    File,
}

/// A running server and what is needed to pause or restart it.
pub struct ScenarioServer {
    /// The settings it was started with.
    settings: Settings,
    /// The port it listens on, kept across pauses and restarts.
    addr: SocketAddr,
    /// The broker, replaced on `restart`.
    broker: SharedBroker,
    /// The server, for serving again after a pause.
    server: Server,
    /// The accept loop while the server is accepting.
    accepting: Option<JoinHandle<()>>,
    /// The broker's QoS 1 retry loop, which holds the broker (and so the
    /// storage) until it is stopped.
    retrying: JoinHandle<()>,
    /// The storage directory, for the file backend.
    dir: Option<TempDir>,
    /// The accounts, kept across restarts.
    users: Arc<dyn UserStore>,
}

/// The settings scenarios run with: the defaults, with a short QoS 1
/// acknowledgement timeout so redelivery happens within a scenario. The
/// retry loop checks once a second, so a resend comes 1 to 1.3 seconds
/// after the original.
///
/// Returns:
///     The settings.
pub fn scenario_settings() -> Settings {
    let mut settings = Settings::default();
    settings.broker.ack_timeout_ms = 300;
    settings
}

impl ScenarioServer {
    /// Starts a server on a free local port.
    ///
    /// Args:
    ///     backend: Where it keeps its data.
    ///
    /// Returns:
    ///     The running server.
    ///
    /// Raises:
    ///     String: When storage or the port can't be opened.
    pub async fn start(backend: Backend) -> Result<Self, String> {
        let settings = scenario_settings();
        let dir = match backend {
            Backend::Memory => None,
            Backend::File => Some(tempfile::tempdir().map_err(|e| e.to_string())?),
        };
        let users: Arc<dyn UserStore> = Arc::new(InMemoryUserStore::new());
        let storage = open_storage(dir.as_ref()).await?;
        let broker = new_broker(&settings, storage)?;
        let server =
            Server::from_settings_with_users(broker.clone(), &settings.server, users.clone()).await;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let accepting = Some(tokio::spawn(server.clone().serve(listener)));
        let retrying = tokio::spawn(broker.clone().run_retry_loop());
        Ok(Self {
            settings,
            addr,
            broker,
            server,
            accepting,
            retrying,
            dir,
            users,
        })
    }

    /// The WebSocket URL clients connect to.
    ///
    /// Returns:
    ///     `ws://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// The broker, for direct publishes and checks.
    ///
    /// Returns:
    ///     The current broker.
    pub fn broker(&self) -> &SharedBroker {
        &self.broker
    }

    /// Stops accepting connections and closes every open one with
    /// `going_away`, keeping the broker and its data.
    pub async fn pause(&mut self) {
        if let Some(accepting) = self.accepting.take() {
            accepting.abort();
            let _ = accepting.await;
        }
        self.broker.close_all(CloseCode::GoingAway);
        for _ in 0..300 {
            if self.broker.stats().connections == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Accepts connections again on the same port after `pause`.
    ///
    /// Raises:
    ///     String: When the port can't be bound again.
    pub async fn resume(&mut self) -> Result<(), String> {
        let listener = bind_again(self.addr).await?;
        self.accepting = Some(tokio::spawn(self.server.clone().serve(listener)));
        Ok(())
    }

    /// Stops the server completely and starts a new one on the same port,
    /// reopening the same storage. With the memory backend the data is gone;
    /// accounts are kept either way.
    ///
    /// Raises:
    ///     String: When the storage or the port can't be opened again.
    pub async fn restart(&mut self) -> Result<(), String> {
        self.pause().await;
        self.retrying.abort();
        let placeholder = new_broker(&self.settings, Arc::new(MemoryStorage::new()))?;
        drop(std::mem::replace(&mut self.broker, placeholder));
        self.server = Server::from_settings_with_users(
            self.broker.clone(),
            &self.settings.server,
            self.users.clone(),
        )
        .await;
        let mut storage = None;
        for _ in 0..500 {
            match open_storage(self.dir.as_ref()).await {
                Ok(opened) => {
                    storage = Some(opened);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        let storage = storage.ok_or("the storage was still locked after five seconds")?;
        self.broker = new_broker(&self.settings, storage)?;
        self.retrying = tokio::spawn(self.broker.clone().run_retry_loop());
        self.server = Server::from_settings_with_users(
            self.broker.clone(),
            &self.settings.server,
            self.users.clone(),
        )
        .await;
        self.resume().await
    }

    /// Publishes a message from the server side, as another client would.
    ///
    /// Args:
    ///     topic: The topic.
    ///     payload: The body.
    ///     qos: 0 or 1.
    ///
    /// Returns:
    ///     The stored record's `seq`.
    ///
    /// Raises:
    ///     String: When storing it fails.
    pub async fn publish(&self, topic: &str, payload: &str, qos: u8) -> Result<u64, String> {
        let record = NewRecord {
            message_id: uuid::Uuid::new_v4().to_string(),
            publisher: "scenario".to_string(),
            payload: payload.to_string(),
            qos,
            published_at_ms: now_ms(),
        };
        self.broker
            .publish(topic, record)
            .await
            .map(|published| published.seq)
            .map_err(|e| e.to_string())
    }

    /// The last `seq` stored on a topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The last `seq`, 0 for an empty log.
    pub async fn last_seq(&self, topic: &str) -> u64 {
        self.broker.last_seq(topic).await.unwrap_or_default()
    }
}

impl Drop for ScenarioServer {
    /// Stops the accept and retry loops, so nothing outlives the scenario.
    fn drop(&mut self) {
        if let Some(accepting) = &self.accepting {
            accepting.abort();
        }
        self.retrying.abort();
    }
}

/// Opens the storage for a server.
///
/// Args:
///     dir: The directory for the file backend, or `None` for memory.
///
/// Returns:
///     The storage.
///
/// Raises:
///     String: When the directory can't be opened, for example while the
///         previous server still holds it.
async fn open_storage(dir: Option<&TempDir>) -> Result<Arc<dyn Storage>, String> {
    match dir {
        None => Ok(Arc::new(MemoryStorage::new())),
        Some(dir) => FileStorage::open(dir.path())
            .await
            .map(|storage| Arc::new(storage) as Arc<dyn Storage>)
            .map_err(|e| e.to_string()),
    }
}

/// Builds a broker over a storage with the scenario settings.
///
/// Args:
///     settings: The settings.
///     storage: The storage.
///
/// Returns:
///     The broker.
///
/// Raises:
///     String: When the storage refuses the durability.
fn new_broker(settings: &Settings, storage: Arc<dyn Storage>) -> Result<SharedBroker, String> {
    SharedBroker::new(
        Broker::new(settings.broker.clone()),
        storage,
        LogSettings {
            durability: Durability::None,
            retention: settings.storage.retention.to_retention(),
            session_expiry: Duration::from_secs(3600),
        },
    )
    .map_err(|e| e.to_string())
}

/// Binds a port again, retrying briefly while the old listener lets go.
///
/// Args:
///     addr: The address.
///
/// Returns:
///     The listener.
///
/// Raises:
///     String: When the port stays taken for two seconds.
async fn bind_again(addr: SocketAddr) -> Result<TcpListener, String> {
    let mut last = String::new();
    for _ in 0..200 {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(format!("can't bind {addr} again: {last}"))
}

/// The current time.
///
/// Returns:
///     Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
