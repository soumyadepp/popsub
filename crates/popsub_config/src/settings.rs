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
    /// The `[limits]` section: rate limits on logins, registration and publishing.
    pub limits: LimitSettings,
    /// The `[telemetry]` section: exporting metrics, traces and logs with OpenTelemetry.
    pub telemetry: TelemetrySettings,
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
    /// How often the server pings each connection, in seconds.
    pub heartbeat_secs: u64,
    /// How long a connection may send nothing, not even a pong, before it is
    /// closed, in seconds. Should be a few heartbeats long.
    pub idle_timeout_secs: u64,
    /// The longest payload a publish may carry, in bytes. WebSocket frames
    /// longer than twice this plus 64 KiB are refused outright.
    pub max_payload_bytes: usize,
    /// The most bytes one connection may have queued for sending. Above a
    /// sixteenth of this, deliveries wait and are caught up from the log;
    /// going over it closes the connection as a slow consumer.
    pub outbound_queue_bytes: usize,
    /// Reverse proxies whose `X-Forwarded-For` header is believed, as IP
    /// addresses or CIDR ranges (`10.0.0.0/8`). Rate limits then key on the
    /// client's address from that header instead of the proxy's. Empty (the
    /// default) believes no one. From the environment, separate entries
    /// with commas.
    #[serde(deserialize_with = "list_or_commas")]
    pub trusted_proxies: Vec<String>,
    /// The most connections one client address may hold at once; more are
    /// closed with `try_again_later` (1013). 0 (the default) means no limit.
    /// Behind a proxy, set `trusted_proxies` first, or every client counts
    /// as the proxy's address.
    pub max_connections_per_address: usize,
    /// Whether connections from `trusted_proxies` start with a PROXY protocol
    /// header (version 1 or 2), as TCP load balancers send, giving the
    /// client's address. Off by default; needs `trusted_proxies`.
    pub proxy_protocol: bool,
    /// Where to serve `/healthz` and `/readyz` over plain HTTP, such as
    /// `127.0.0.1:9091`. Empty (the default) serves neither.
    pub health_address: String,
    /// PEM file with the TLS certificate chain, leaf first. Set it with
    /// `tls_key` to serve `wss://`; both empty (the default) serves `ws://`.
    pub tls_cert: String,
    /// PEM file with the TLS private key for `tls_cert`.
    pub tls_key: String,
}

impl Default for ServerSettings {
    /// Builds the server defaults: loopback on port 8080, a published JWT
    /// secret and admin password (fine only for local development), 24 hour
    /// tokens, registration off, a ping every 30 seconds, a 90 second idle
    /// timeout, 1 MiB payloads, a 16 MiB outbound queue per connection, no
    /// trusted proxies, no PROXY protocol, no per-address connection limit,
    /// no TLS and no health endpoints.
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
            heartbeat_secs: 30,
            idle_timeout_secs: 90,
            max_payload_bytes: 1024 * 1024,
            outbound_queue_bytes: 16 * 1024 * 1024,
            trusted_proxies: Vec::new(),
            max_connections_per_address: 0,
            proxy_protocol: false,
            health_address: String::new(),
            tls_cert: String::new(),
            tls_key: String::new(),
        }
    }
}

/// The `[broker]` section of the configuration.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct BrokerSettings {
    /// The most connections the server holds at once, counted from the
    /// moment a TCP connection is accepted. Connections over it are closed
    /// with `try_again_later` (1013). 0 means no limit.
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

/// The `[limits]` section of the configuration: rate limits on logins,
/// registration and publishing.
///
/// Values below 1 are treated as 1, so a limit can be made strict but never
/// turned off by accident.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct LimitSettings {
    /// Failed logins allowed for one address and username pair before it is locked out.
    pub login_attempts: u32,
    /// How long that pair's first lockout lasts, in seconds.
    pub login_lockout_secs: u64,
    /// Failed logins allowed from one address, across all usernames, before
    /// the address is locked out.
    pub address_login_attempts: u32,
    /// How long that address's first lockout lasts, in seconds.
    pub address_lockout_secs: u64,
    /// The longest a lockout grows to. Each repeat lockout doubles, up to this, in seconds.
    pub max_lockout_secs: u64,
    /// Registration attempts allowed from one address per hour, successful or not.
    pub registrations_per_hour: u64,
    /// Messages one user may publish per second, across all their connections.
    pub publish_per_second: u64,
    /// How many publishes a user may send at once before the per-second rate applies.
    pub publish_burst: u64,
}

impl Default for LimitSettings {
    /// Builds the default limits: 5 failed logins per address and username
    /// (5 minute lockout), 20 per address (15 minutes), lockouts growing to
    /// an hour, 5 registrations per address per hour, and 100 publishes per
    /// second per user with bursts of 50.
    ///
    /// Returns:
    ///     The default `[limits]` section.
    fn default() -> Self {
        Self {
            login_attempts: 5,
            login_lockout_secs: 300,
            address_login_attempts: 20,
            address_lockout_secs: 900,
            max_lockout_secs: 3600,
            registrations_per_hour: 5,
            publish_per_second: 100,
            publish_burst: 50,
        }
    }
}

/// The `[telemetry]` section: OpenTelemetry metrics, traces and logs,
/// exported over OTLP (HTTP and protobuf).
///
/// Where they go is set with the standard OpenTelemetry environment
/// variables, such as `OTEL_EXPORTER_OTLP_ENDPOINT` (default
/// `http://localhost:4318`), `OTEL_SERVICE_NAME` and
/// `OTEL_RESOURCE_ATTRIBUTES`.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct TelemetrySettings {
    /// Whether to export anything. Off by default.
    pub enabled: bool,
    /// The share of request traces to keep, from 0 to 1. A trace whose
    /// parent was sampled is always kept.
    pub sample_ratio: f64,
}

impl Default for TelemetrySettings {
    /// Builds the defaults: nothing exported, every trace kept once enabled.
    ///
    /// Returns:
    ///     The default `[telemetry]` section.
    fn default() -> Self {
        Self {
            enabled: false,
            sample_ratio: 1.0,
        }
    }
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

/// Reads a list of strings written either as a list or, as environment
/// variables must be, as one comma-separated string. Blank entries are dropped.
///
/// Args:
///     deserializer: The value's deserializer.
///
/// Returns:
///     The entries, trimmed.
///
/// Raises:
///     D::Error: When the value is neither a string nor a list of strings.
fn list_or_commas<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// The two ways the value may be written.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ListOrCommas {
        /// A list, as in a TOML file.
        List(Vec<String>),
        /// One comma-separated string, as in an environment variable.
        Commas(String),
    }
    let entries = match ListOrCommas::deserialize(deserializer)? {
        ListOrCommas::List(list) => list,
        ListOrCommas::Commas(text) => text.split(',').map(str::to_string).collect(),
    };
    Ok(entries
        .into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect())
}

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
