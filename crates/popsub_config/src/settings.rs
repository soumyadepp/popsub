//! Setting types for the server and broker, their defaults, and checks for
//! values that ship with PopSub.
//!
//! Each struct is marked `#[serde(default)]`, so a config file or the
//! environment only has to name the values it changes; everything else comes
//! from the struct's `Default`.

use serde::Deserialize;

/// Every setting PopSub reads, grouped by section.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Settings {
    /// The `[server]` section: listening address, auth and registration.
    pub server: ServerSettings,
    /// The `[broker]` section: delivery and storage behaviour.
    pub broker: BrokerSettings,
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
    /// How long stored messages are kept for replay, in seconds.
    pub message_ttl_secs: u64,
    /// How long to wait for a QoS 1 acknowledgement before resending, in milliseconds.
    pub ack_timeout_ms: u64,
    /// How many times a QoS 1 message is resent before it is dropped.
    pub max_retries: u8,
}

impl Default for BrokerSettings {
    /// Builds the broker defaults: 1000 connections, one hour of history, a
    /// 5 second ACK timeout and 5 retries.
    ///
    /// Returns:
    ///     The default `[broker]` section.
    fn default() -> Self {
        Self {
            max_connections: 1000,
            message_ttl_secs: 3600,
            ack_timeout_ms: 5000,
            max_retries: 5,
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
