//! Command-line entry point for PopSub.
//!
//! The `server` subcommand loads settings, opens the configured storage
//! backend, builds the broker, spawns its QoS 1 retry, retention and session
//! expiry loops, serves the health endpoints when configured, and runs the
//! WebSocket server until Ctrl+C or `SIGTERM`. The `client`
//! subcommand runs a short demo session (connect, subscribe, publish)
//! against a running server with the SDK (`popsub_sdk`), for smoke tests.
//! The `users` subcommands manage accounts on a running server (see `users`).
//! Everything here is wiring; the protocol, routing and storage live in the
//! transport, broker and storage crates.

mod telemetry;
mod users;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use telemetry::Telemetry;

use clap::Parser;
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::{Settings, StorageBackend, StorageSettings, load_config};
use popsub_protocol::CloseCode;
use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};
use popsub_storage::{FileStorage, MemoryStorage, Storage};
use popsub_transport::{
    FileUserStore, Health, InMemoryUserStore, RateLimiters, Server, ServerTls, TrustedProxies,
    UserStore,
};
use tracing::{error, info, warn};

/// How long a shutdown waits after sending every client a close frame, so
/// the frames are written before the process exits.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// Runs the PopSub WebSocket broker or a demo client against it.
#[derive(Parser)]
#[command(name = "popsub")]
enum Command {
    /// Start the WebSocket server
    Server,
    /// Log in, subscribe to a topic, publish to it and print what comes back
    Client {
        /// WebSocket URL of the server
        #[arg(long, default_value = "ws://127.0.0.1:8080")]
        url: String,
        /// Username to log in as
        #[arg(long, default_value = "admin")]
        username: String,
        /// Password to log in with
        #[arg(long, default_value = "password")]
        password: String,
        /// Topic to subscribe and publish to
        #[arg(long, default_value = "chat")]
        topic: String,
        /// PEM certificate to trust for wss://, such as a self-signed one
        #[arg(long)]
        ca_cert: Option<std::path::PathBuf>,
    },
    /// Manage accounts on a running server, as an admin
    Users {
        /// Where to connect and who to log in as
        #[command(flatten)]
        connection: users::Connection,
        /// The action
        #[command(subcommand)]
        command: users::UsersCommand,
    },
}

/// Parses the command line, starts logging and runs the chosen subcommand.
///
/// `server` sets up logging and telemetry from its settings (see
/// `start_server`); `client` logs at `info`, and `users` at `warn` so its
/// output stays readable.
///
/// Returns:
///     Success when the subcommand finishes cleanly, failure (after logging
///     the error) when it fails, so scripts and process supervisors can tell.
#[tokio::main]
async fn main() -> ExitCode {
    let result = match Command::parse() {
        Command::Server => return start_server().await,
        Command::Client {
            url,
            username,
            password,
            topic,
            ca_cert,
        } => {
            popsub_utils::logging::init("info");
            run_client(&url, &username, &password, &topic, ca_cert.as_deref()).await
        }
        Command::Users {
            connection,
            command,
        } => {
            popsub_utils::logging::init("warn");
            users::run(connection, command).await
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Loads settings, starts logging and telemetry from them, runs the server,
/// and flushes telemetry before exiting.
///
/// Settings are loaded before logging starts, because `[telemetry]` decides
/// how logging is set up; a settings error is printed to standard error.
///
/// Returns:
///     Success when the server stopped cleanly, failure (after logging why) otherwise.
async fn start_server() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("can't load the configuration: {e}");
            return ExitCode::FAILURE;
        }
    };
    popsub_utils::pii::set_masking(config.telemetry.mask_pii);
    let telemetry = match Telemetry::init(tracing::Level::INFO, &config.telemetry) {
        Ok(telemetry) => telemetry,
        Err(e) => {
            popsub_utils::logging::init("info");
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if !config.telemetry.mask_pii {
        warn!(
            "telemetry.mask_pii is off: usernames and client addresses appear in logs and traces"
        );
    }
    if config.telemetry.enabled {
        info!(
            sample_ratio = config.telemetry.sample_ratio,
            "exporting OpenTelemetry metrics, traces and logs over OTLP"
        );
    }
    let mut telemetry = telemetry;
    let outcome = run_server(config, &mut telemetry).await;
    if let Err(e) = &outcome {
        error!("{e}");
    }
    tokio::task::block_in_place(|| telemetry.shutdown());
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Runs the broker and WebSocket server until Ctrl+C or `SIGTERM`.
///
/// If the JWT secret or admin password still has a published default value,
/// the server refuses to start on a non-loopback host and only logs a warning
/// for each such setting on `localhost` or a loopback address. Otherwise it
/// opens the `[storage]` backend and the user store that goes with it,
/// applies the `[limits]` rate limits and the trusted proxies,
/// builds the broker (refusing a durability
/// the backend can't provide), spawns the QoS 1 retry, retention and session
/// expiry loops,
/// serves `/healthz` and `/readyz` on `server.health_address` when set,
/// and serves on `host:port` until Ctrl+C or `SIGTERM`. On either,
/// `/readyz` turns to 503 and every client is sent a `going_away` close
/// frame before the process exits.
///
/// Args:
///     config: The loaded settings.
///     telemetry: The telemetry providers, to register the broker's gauges with.
///
/// Raises:
///     String: When published default secrets are set and the host is not a loopback address.
///     String: When an entry in `server.trusted_proxies` isn't an IP address or CIDR range.
///     String: When `server.proxy_protocol` is on with no trusted proxies.
///     String: When only one of `server.tls_cert` and `server.tls_key` is set,
///         or they can't be read or don't match.
///     String: When the storage backend can't be opened, for example because
///         another server is using its directory.
///     String: When `users.json` in the storage directory exists but can't be
///         read or parsed.
///     String: When `storage.durability` is stronger than the backend provides.
///     String: When a `storage.topic_retention` pattern is malformed.
///     String: When `host:port` or `server.health_address` can't be bound,
///         with the underlying I/O error.
async fn run_server(
    config: Settings,
    telemetry: &mut Telemetry,
) -> Result<(), Box<dyn std::error::Error>> {
    let insecure = config.insecure_defaults();
    if !insecure.is_empty() {
        if !config.server.is_loopback() {
            return Err(format!(
                "refusing to listen on {} with published defaults ({}). Set them in config/default.toml, or with SERVER__JWT_SECRET and SERVER_PASSWORD.",
                config.server.host,
                insecure.join(", ")
            )
            .into());
        }
        for problem in &insecure {
            warn!("{problem}. Fine for local development, never for a public address.");
        }
    }
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let tls = open_tls(&config.server.tls_cert, &config.server.tls_key)?;
    let storage = open_storage(&config.storage).await?;
    let log = LogSettings {
        durability: config.storage.durability,
        retention: config.storage.retention.to_retention(),
        session_expiry: Duration::from_secs(config.storage.session_expiry_secs),
    };
    let topic_retention = config.storage.topic_retention_rules()?;
    let broker = SharedBroker::new(Broker::new(config.broker.clone()), storage, log)
        .map_err(|e| {
            format!(
                "can't use the {:?} storage backend: {e}",
                config.storage.backend
            )
        })?
        .with_topic_retention(topic_retention);
    info!(backend = ?config.storage.backend, durability = ?log.durability, "storage opened");
    telemetry.register_gauges(&broker);
    tokio::spawn(broker.clone().run_retry_loop());
    tokio::spawn(broker.clone().run_retention_loop());
    tokio::spawn(broker.clone().run_session_expiry_loop());
    let users = open_users(&config.storage)?;
    let proxies = TrustedProxies::parse(&config.server.trusted_proxies)
        .map_err(|e| format!("server.trusted_proxies: {e}"))?;
    if config.server.proxy_protocol && proxies.is_empty() {
        return Err(
            "server.proxy_protocol is on but server.trusted_proxies is empty; list the load balancers that send the header"
                .into(),
        );
    }
    if !proxies.is_empty() {
        info!(
            proxies = config.server.trusted_proxies.len(),
            "believing X-Forwarded-For from trusted proxies"
        );
    }
    let mut server = Server::from_settings_with_users(broker.clone(), &config.server, users)
        .await
        .with_rate_limiters(RateLimiters::from_settings(&config.limits))
        .with_trusted_proxies(proxies);
    if let Some(tls) = tls {
        server = server.with_tls(tls);
    }

    let health = Health::new(broker.clone());
    let _health_task = match config.server.health_address.as_str() {
        "" => None,
        health_addr => {
            let listener = tokio::net::TcpListener::bind(health_addr)
                .await
                .map_err(|e| format!("can't serve health checks on {health_addr}: {e}"))?;
            Some(AbortOnDrop(tokio::spawn(health.clone().serve(listener))))
        }
    };

    tokio::select! {
        served = server.listen(&addr) => served.map_err(|e| format!("can't serve on {addr}: {e}"))?,
        signal = shutdown_signal() => {
            health.drain();
            let closed = broker.close_all(CloseCode::GoingAway);
            info!(signal, closed, "shutting down, closing connections");
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        }
    }
    Ok(())
}

/// Aborts a spawned task when dropped, so it stops with `run_server`.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    /// Aborts the task.
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Waits for a request to shut down: Ctrl+C, or `SIGTERM` on Unix, which is
/// what `docker stop` and Kubernetes send.
///
/// Returns:
///     The signal's name, for the log.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => "SIGINT",
                _ = terminate.recv() => "SIGTERM",
            },
            Err(e) => {
                warn!(%e, "can't listen for SIGTERM; only Ctrl+C shuts down cleanly");
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl+C"
    }
}

/// Opens the storage backend the `[storage]` section names.
///
/// Args:
///     settings: The `[storage]` section.
///
/// Returns:
///     The backend, ready to share.
///
/// Raises:
///     String: When the `file` backend's directory can't be opened, for
///         example because another server is using it, or holds damaged data.
async fn open_storage(settings: &StorageSettings) -> Result<Arc<dyn Storage>, String> {
    match settings.backend {
        StorageBackend::File => {
            let storage = FileStorage::open(&settings.path)
                .await
                .map_err(|e| format!("can't open the message store in {}: {e}", settings.path))?;
            info!(path = %settings.path, "file storage opened");
            Ok(Arc::new(storage))
        }
        StorageBackend::Memory => {
            warn!(
                "storage backend is memory: topic history and sessions are lost when the server stops"
            );
            Ok(Arc::new(MemoryStorage::new()))
        }
    }
}

/// Loads the TLS certificate and key when both are configured.
///
/// Args:
///     cert: `server.tls_cert`, empty for none.
///     key: `server.tls_key`, empty for none.
///
/// Returns:
///     The TLS setup, or `None` when neither is set.
///
/// Raises:
///     String: When only one is set, or the files can't be read or don't match.
fn open_tls(cert: &str, key: &str) -> Result<Option<ServerTls>, String> {
    match (cert.is_empty(), key.is_empty()) {
        (true, true) => Ok(None),
        (false, false) => {
            let tls = ServerTls::from_pem_files(Path::new(cert), Path::new(key))
                .map_err(|e| format!("can't load the TLS certificate: {e}"))?;
            info!(cert, "TLS enabled");
            Ok(Some(tls))
        }
        _ => Err("set both server.tls_cert and server.tls_key, or neither".to_string()),
    }
}

/// Opens the user store that goes with the `[storage]` backend.
///
/// With the `file` backend, accounts live in `users.json` in the storage
/// directory and survive restarts; the directory's lock (taken when the
/// storage opened) also keeps other servers away from it. With the `memory`
/// backend they live in memory and are lost on restart.
///
/// Args:
///     settings: The `[storage]` section.
///
/// Returns:
///     The user store.
///
/// Raises:
///     String: When `users.json` exists but can't be read or parsed.
fn open_users(settings: &StorageSettings) -> Result<Arc<dyn UserStore>, String> {
    match settings.backend {
        StorageBackend::File => {
            let path = std::path::Path::new(&settings.path).join("users.json");
            let store = FileUserStore::open(&path)
                .map_err(|e| format!("can't open the user store: {e}"))?;
            info!(path = %path.display(), "user store opened");
            Ok(Arc::new(store))
        }
        StorageBackend::Memory => {
            warn!(
                "users are kept in memory: accounts created while running are lost when the server stops"
            );
            Ok(Arc::new(InMemoryUserStore::new()))
        }
    }
}

/// Runs a short session against a PopSub server with the SDK and prints
/// what happens.
///
/// Connects (logging in and authenticating), subscribes to `topic`,
/// publishes `Hello from popsub client` to it with confirmation, and prints
/// the delivered message with its `seq`. The defaults match the default
/// server settings; pass `--username` and `--password` for any other account.
///
/// Args:
///     url: WebSocket URL of the server, such as `ws://127.0.0.1:8080`.
///     username: The account to log in as.
///     password: The account's password.
///     topic: The topic to subscribe and publish to.
///     ca_cert: A PEM certificate to trust for `wss://`, if any.
///
/// Raises:
///     popsub_sdk::Error: When connecting, subscribing or publishing fails.
///     String: When the `ca_cert` file can't be read.
///     String: When the subscription ends before the message arrives.
async fn run_client(
    url: &str,
    username: &str,
    password: &str,
    topic: &str,
    ca_cert: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = trust(
        ClientConfig::new(url, Credentials::password(username, password))
            .request_timeout(Duration::from_secs(10)),
        ca_cert,
    )?;
    let client = Client::connect(config).await?;
    println!("Logged in as {username}");
    println!("Authenticated");
    let mut subscription = client.subscribe(topic, StartFrom::New).await?;
    println!("Subscribed to {topic}");
    let stored = client
        .publish_confirmed(topic, "Hello from popsub client")
        .await?;
    println!("Published as seq {}", stored.seq);
    let message = subscription
        .next()
        .await
        .ok_or("the subscription ended before the message arrived")?;
    println!(
        "Received on {topic} (seq {}): {}",
        message.seq, message.payload
    );
    client.close();
    Ok(())
}

/// Adds a certificate file to a client's trusted roots, when one is given.
///
/// Args:
///     config: The client configuration.
///     ca_cert: A PEM certificate file, or `None`.
///
/// Returns:
///     The configuration.
///
/// Raises:
///     String: When the file can't be read.
///     popsub_sdk::Error: When it holds no usable certificate.
fn trust(
    config: ClientConfig,
    ca_cert: Option<&Path>,
) -> Result<ClientConfig, Box<dyn std::error::Error>> {
    let Some(path) = ca_cert else {
        return Ok(config);
    };
    let pem = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    Ok(config.add_root_certificates_pem(&pem)?)
}
