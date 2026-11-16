//! Setting types for the server and broker, their defaults, and checks for
//! values that ship with PopSub.
//!
//! Each struct is marked `#[serde(default)]`, so a config file or the
//! environment only has to name the values it changes; everything else comes
//! from the struct's `Default`.

use popsub_storage::{Durability, Retention};
use serde::Deserialize;

/// Every setting PopSub reads, grouped by section.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Settings {
    /// The `[server]` section: listening address, auth and registration.
    pub server: ServerSettings,
    /// The `[broker]` section: QoS 1 delivery behaviour.
    pub broker: BrokerSettings,
    /// The `[storage]` section: which backend holds topic logs, how firmly
    /// appends are stored, and how much history is kept.
    pub storage: StorageSettings,
}

/// The `[server]` section of the configuration.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ServerSettings {
    /// Address the WebSocket server binds to.
    pub host: String,
    /// Port the WebSocket server binds to.
    pub port: u16,
    /// Secret used to sign and verify JWTs.
    pub jwt_secret: String,
    /// How long an issued JWT stays valid, in hours.
    pub jwt_expiration_hours: u64,
    /// Username of the admin account created at startup.
    pub username: String,
    /// Password of the admin account created at startup.
    pub password: String,
    /// Whether clients may create their own accounts with `register`.
    pub allow_registration: bool,
    /// Topics given to self-registered users: `none`, `readonly` or `full`.
    pub registration_role: String,
}

impl Default for ServerSettings {
    /// Builds the server defaults: loopback on port 8080, a published JWT
    /// secret and admin password (fine only for local development), 24 hour
    /// tokens, and registration off.
    ///
    /// Returns:
    ///     The default `[server]` section.
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            jwt_secret: "default_secret".to_string(),
            jwt_expiration_hours: 24,
            username: "admin".to_string(),
            password: DEFAULT_PASSWORD.to_string(),
            allow_registration: false,
            registration_role: "none".to_string(),
        }
    }
}

/// The `[broker]` section of the configuration.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct BrokerSettings {
    /// Intended cap on simultaneous connections. Not enforced yet.
    pub max_connections: usize,
    /// How long to wait for a QoS 1 acknowledgement before resending, in milliseconds.
    pub ack_timeout_ms: u64,
    /// How many times a QoS 1 message is resent before the subscription is
    /// reported as stalled and delivery to it pauses.
    pub max_retries: u8,
    /// The most QoS 1 records a subscription may have sent but unacknowledged.
    /// When full, delivery to that subscription waits for acks. Values below
    /// 1 are treated as 1.
    pub max_inflight: usize,
}

impl Default for BrokerSettings {
    /// Builds the broker defaults: 1000 connections, a 5 second ACK timeout,
    /// 5 retries and 100 unacknowledged records per subscription.
    ///
    /// Returns:
    ///     The default `[broker]` section.
    fn default() -> Self {
        Self {
            max_connections: 1000,
            ack_timeout_ms: 5000,
            max_retries: 5,
            max_inflight: 100,
        }
    }
}

/// The storage backends PopSub can open from configuration.
///
/// Other backends can still be used by building the server in code with any
/// `popsub_storage::Storage`.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    /// `popsub_storage::FileStorage`: topic logs and sessions in the
    /// `path` directory, kept across restarts.
    File,
    /// `popsub_storage::MemoryStorage`: history and sessions live in the
    /// process and are lost on restart.
    Memory,
}

/// The `[storage]` section of the configuration.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct StorageSettings {
    /// Which backend holds topic logs.
    pub backend: StorageBackend,
    /// The directory the `file` backend uses, relative to the working
    /// directory unless absolute. Ignored by other backends.
    pub path: String,
    /// How firmly each publish is stored before it counts as done. Startup
    /// fails if the backend can't provide it.
    pub durability: Durability,
    /// How much history each topic keeps.
    pub retention: RetentionSettings,
    /// How long a durable session's subscriptions and cursors are kept after
    /// its last connection ends, in seconds.
    pub session_expiry_secs: u64,
}

impl Default for StorageSettings {
    /// Builds the storage defaults: the file backend in `pubsub_data`,
    /// buffered durability, the default retention, and sessions kept for a day.
    ///
    /// Returns:
    ///     The default `[storage]` section.
    fn default() -> Self {
        Self {
            backend: StorageBackend::File,
            path: "pubsub_data".to_string(),
            durability: Durability::Buffered,
            retention: RetentionSettings::default(),
            session_expiry_secs: 86_400,
        }
    }
}

/// The `[storage.retention]` section: limits applied to every topic log.
///
/// A record is trimmed once it is outside any limit. Setting a limit to 0
/// keeps nothing; there is no way to switch a limit off from a file, because
/// unbounded history is never what a broker wants by accident.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(default)]
pub struct RetentionSettings {
    /// The most records kept per topic.
    pub max_messages: u64,
    /// How long a record is kept, in seconds.
    pub max_age_secs: u64,
    /// The most bytes kept per topic, counting message ids, publishers and payloads.
    pub max_bytes: u64,
}

impl Default for RetentionSettings {
    /// Builds the retention defaults: 1000 records, one hour, 64 MiB.
    ///
    /// Returns:
    ///     The default `[storage.retention]` section.
    fn default() -> Self {
        Self {
            max_messages: 1000,
            max_age_secs: 3600,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

impl RetentionSettings {
    /// Converts the settings into the policy the storage backends apply.
    ///
    /// Returns:
    ///     A `Retention` with every limit set.
    pub fn to_retention(&self) -> Retention {
        Retention {
            max_messages: Some(self.max_messages),
            max_age_ms: Some(self.max_age_secs.saturating_mul(1000)),
            max_bytes: Some(self.max_bytes),
        }
    }
}

/// JWT secrets that appear in PopSub's code or sample config.
pub const KNOWN_DEFAULT_SECRETS: [&str; 3] = [
    "default_secret",
    "a_very_secret_key",
    "change_me_in_production",
];

/// The admin password that appears in PopSub's code and sample config.
pub const DEFAULT_PASSWORD: &str = "password";

impl Settings {
    /// Lists settings still using values that ship with PopSub.
    ///
    /// Returns:
    ///     A description of each insecure setting, empty when there are none.
    pub fn insecure_defaults(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if KNOWN_DEFAULT_SECRETS.contains(&self.server.jwt_secret.as_str()) {
            found.push("server.jwt_secret is a published default");
        }
        if self.server.password == DEFAULT_PASSWORD {
            found.push("server.password is the published default");
        }
        found
    }
}

impl ServerSettings {
    /// Whether the server only accepts connections from this machine.
    ///
    /// Returns:
    ///     True for `localhost` and loopback addresses such as `127.0.0.1` or `::1`.
    pub fn is_loopback(&self) -> bool {
        self.host == "localhost"
            || self
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}
