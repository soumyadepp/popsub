//! Setting types for the server and broker, their defaults, and checks for
//! values that ship with PopSub.
//!
//! `Settings` is what the rest of the workspace reads. The `Partial` structs
//! mirror it with every field optional so `load_config` can tell which values
//! a source actually set before filling the rest from `Settings::default()`.

use serde::Deserialize;

/// All PopSub settings, after merging the file, the environment and defaults.
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    /// Listener, authentication and account settings.
    pub server: ServerSettings,
    /// Delivery and retry settings for the broker.
    pub broker: BrokerSettings,
}

/// Settings for the WebSocket listener, JWTs and the built-in admin account.
#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    /// Address the WebSocket server binds to.
    pub host: String,
    /// TCP port the WebSocket server listens on.
    pub port: u16,
    /// Secret used to sign and verify JWTs.
    pub jwt_secret: String,
    /// How many hours a JWT issued at login stays valid.
    pub jwt_expiration_hours: u64,
    /// Username of the built-in admin account.
    pub username: String,
    /// Password of the built-in admin account.
    pub password: String,
    /// Whether clients may create their own accounts with `register`.
    pub allow_registration: bool,
    /// Topics given to self-registered users: `none`, `readonly` or `full`.
    pub registration_role: String,
}

/// Settings for the broker's QoS 1 retries and connection limits.
#[derive(Debug, Deserialize, Clone)]
pub struct BrokerSettings {
    /// Intended cap on concurrent connections; loaded but not read by anything yet.
    pub max_connections: usize,
    /// Intended message lifetime in seconds; loaded but not read (persistence uses its own TTL).
    pub message_ttl_secs: u64,
    /// Milliseconds to wait for a QoS 1 acknowledgement before resending.
    pub ack_timeout_ms: u64,
    /// How many times an unacknowledged QoS 1 message is resent before it is dropped.
    pub max_retries: u8,
}

/// Settings as read from the file and environment, before defaults are applied.
#[derive(Debug, Deserialize)]
pub struct PartialSettings {
    /// The `[server]` section, if any source set one of its keys.
    pub server: Option<PartialServerSettings>,
    /// The `[broker]` section, if any source set one of its keys.
    pub broker: Option<PartialBrokerSettings>,
}

/// `ServerSettings` with every field optional, as read from the sources.
#[derive(Debug, Deserialize)]
pub struct PartialServerSettings {
    /// Overrides `server.host` when set.
    pub host: Option<String>,
    /// Overrides `server.port` when set.
    pub port: Option<u16>,
    /// Overrides `server.jwt_secret` when set.
    pub jwt_secret: Option<String>,
    /// Overrides `server.jwt_expiration_hours` when set.
    pub jwt_expiration_hours: Option<u64>,
    /// Overrides `server.username` when set.
    pub username: Option<String>,
    /// Overrides `server.password` when set.
    pub password: Option<String>,
    /// Overrides `server.allow_registration` when set.
    pub allow_registration: Option<bool>,
    /// Overrides `server.registration_role` when set.
    pub registration_role: Option<String>,
}

/// `BrokerSettings` with every field optional, as read from the sources.
#[derive(Debug, Deserialize)]
pub struct PartialBrokerSettings {
    /// Overrides `broker.max_connections` when set.
    pub max_connections: Option<usize>,
    /// Overrides `broker.message_ttl_secs` when set.
    pub message_ttl_secs: Option<u64>,
    /// Overrides `broker.ack_timeout_ms` when set.
    pub ack_timeout_ms: Option<u64>,
    /// Overrides `broker.max_retries` when set.
    pub max_retries: Option<u8>,
}

/// JWT secrets that ship in the code or the sample config.
pub const KNOWN_DEFAULT_SECRETS: [&str; 3] = [
    "default_secret",
    "a_very_secret_key",
    "change_me_in_production",
];

/// The admin password that ships in the code and the sample config.
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
    ///     True for `localhost` and loopback addresses such as `127.0.0.1`.
    pub fn is_loopback(&self) -> bool {
        self.host == "localhost"
            || self
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

impl Default for Settings {
    /// Builds the built-in defaults: `127.0.0.1:8080`, admin `admin` with
    /// password `password`, JWT secret `default_secret` valid for 24 hours,
    /// registration off with role `none`, and a 5000 ms ack timeout with 5
    /// retries.
    ///
    /// The secret and password are published values, so
    /// `insecure_defaults` reports both.
    ///
    /// Returns:
    ///     The default settings.
    fn default() -> Self {
        Self {
            server: ServerSettings {
                host: "127.0.0.1".to_string(),
                port: 8080,
                jwt_secret: "default_secret".to_string(),
                jwt_expiration_hours: 24,
                username: "admin".to_string(),
                password: "password".to_string(),
                allow_registration: false,
                registration_role: "none".to_string(),
            },
            broker: BrokerSettings {
                max_connections: 1000,
                message_ttl_secs: 3600,
                ack_timeout_ms: 5000,
                max_retries: 5,
            },
        }
    }
}
