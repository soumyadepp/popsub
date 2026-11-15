use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub server: ServerSettings,
    pub broker: BrokerSettings,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub jwt_secret: String,
    pub jwt_expiration_hours: u64,
    pub username: String,
    pub password: String,
    /// Whether clients may create their own accounts with `register`.
    pub allow_registration: bool,
    /// Topics given to self-registered users: `none`, `readonly` or `full`.
    pub registration_role: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrokerSettings {
    pub max_connections: usize,
    pub message_ttl_secs: u64,
    pub ack_timeout_ms: u64,
    pub max_retries: u8,
}

#[derive(Debug, Deserialize)]
pub struct PartialSettings {
    pub server: Option<PartialServerSettings>,
    pub broker: Option<PartialBrokerSettings>,
}

#[derive(Debug, Deserialize)]
pub struct PartialServerSettings {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub jwt_secret: Option<String>,
    pub jwt_expiration_hours: Option<u64>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub allow_registration: Option<bool>,
    pub registration_role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PartialBrokerSettings {
    pub max_connections: Option<usize>,
    pub message_ttl_secs: Option<u64>,
    pub ack_timeout_ms: Option<u64>,
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
    ///   A description of each insecure setting, empty when there are none.
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
    ///   True for `localhost` and loopback addresses such as `127.0.0.1`.
    pub fn is_loopback(&self) -> bool {
        self.host == "localhost"
            || self
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

impl Default for Settings {
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
