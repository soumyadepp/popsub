//! Command-line entry point for PopSub.
//!
//! The `server` subcommand loads settings, opens the configured storage
//! backend, builds the broker, spawns its QoS 1 retry, retention and session
//! expiry loops and runs the WebSocket server until Ctrl+C. The `client`
//! subcommand runs a short demo session (connect, subscribe, publish)
//! against a running server with the SDK (`popsub_sdk`), for smoke tests.
//! The `users` subcommands manage accounts on a running server (see `users`).
//! Everything here is wiring; the protocol, routing and storage live in the
//! transport, broker and storage crates.

mod users;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use popsub_broker::{Broker, LogSettings, SharedBroker};
use popsub_config::{StorageBackend, StorageSettings, load_config};
use popsub_protocol::CloseCode;
use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};
use popsub_storage::{FileStorage, MemoryStorage, Storage};
use popsub_transport::{
    FileUserStore, InMemoryUserStore, RateLimiters, Server, TrustedProxies, UserStore,
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

/// Parses the command line, starts logging (at `info`, or `warn` for
/// `users` so its output stays readable) and runs the chosen subcommand.
///
/// Returns:
///     Success when the subcommand finishes cleanly, failure (after logging
///     the error) when it fails, so scripts and process supervisors can tell.
#[tokio::main]
async fn main() -> ExitCode {
    let command = Command::parse();
    let level = match command {
        Command::Users { .. } => "warn",
        _ => "info",
    };
    popsub_utils::logging::init(level);

    let result = match command {
        Command::Server => run_server().await,
        Command::Client {
            url,
            username,
            password,
            topic,
        } => run_client(&url, &username, &password, &topic).await,
        Command::Users {
            connection,
            command,
        } => users::run(connection, command).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Loads settings and runs the broker and WebSocket server until Ctrl+C.
///
/// If the JWT secret or admin password still has a published default value,
/// the server refuses to start on a non-loopback host and only logs a warning
/// for each such setting on `localhost` or a loopback address. Otherwise it
/// opens the `[storage]` backend and the user store that goes with it,
/// applies the `[limits]` rate limits and the trusted proxies,
/// builds the broker (refusing a durability
/// the backend can't provide), spawns the QoS 1 retry, retention and session
/// expiry loops,
/// and serves on `host:port` until Ctrl+C arrives. On Ctrl+C every client is
/// sent a `going_away` close frame before the process exits.
///
/// Raises:
///     ConfigError::FileParse: When `config/default.toml` exists but can't be parsed.
///     ConfigError::Type: When a setting from the file or the environment has
///         the wrong type, including an unknown backend or durability name.
///     String: When published default secrets are set and the host is not a loopback address.
///     String: When an entry in `server.trusted_proxies` isn't an IP address or CIDR range.
///     String: When the storage backend can't be opened, for example because
///         another server is using its directory.
///     String: When `users.json` in the storage directory exists but can't be
///         read or parsed.
///     String: When `storage.durability` is stronger than the backend provides.
///     String: When `host:port` can't be bound, with the underlying I/O error.
async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
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
    let storage = open_storage(&config.storage).await?;
    let log = LogSettings {
        durability: config.storage.durability,
        retention: config.storage.retention.to_retention(),
        session_expiry: Duration::from_secs(config.storage.session_expiry_secs),
    };
    let broker =
        SharedBroker::new(Broker::new(config.broker.clone()), storage, log).map_err(|e| {
            format!(
                "can't use the {:?} storage backend: {e}",
                config.storage.backend
            )
        })?;
    info!(backend = ?config.storage.backend, durability = ?log.durability, "storage opened");
    tokio::spawn(broker.clone().run_retry_loop());
    tokio::spawn(broker.clone().run_retention_loop());
    tokio::spawn(broker.clone().run_session_expiry_loop());
    let users = open_users(&config.storage)?;
    let proxies = TrustedProxies::parse(&config.server.trusted_proxies)
        .map_err(|e| format!("server.trusted_proxies: {e}"))?;
    if !proxies.is_empty() {
        info!(
            proxies = config.server.trusted_proxies.len(),
            "believing X-Forwarded-For from trusted proxies"
        );
    }
    let server = Server::from_settings_with_users(broker.clone(), &config.server, users)
        .await
        .with_rate_limiters(RateLimiters::from_settings(&config.limits))
        .with_trusted_proxies(proxies);

    tokio::select! {
        served = server.listen(&addr) => served.map_err(|e| format!("can't serve on {addr}: {e}"))?,
        _ = tokio::signal::ctrl_c() => {
            let closed = broker.close_all(CloseCode::GoingAway);
            info!(closed, "shutdown signal received, closing connections");
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        }
    }
    Ok(())
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
///
/// Raises:
///     popsub_sdk::Error: When connecting, subscribing or publishing fails.
///     String: When the subscription ends before the message arrives.
async fn run_client(
    url: &str,
    username: &str,
    password: &str,
    topic: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ClientConfig::new(url, Credentials::password(username, password))
        .request_timeout(Duration::from_secs(10));
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
