use serde::Deserialize;

/// Top-level configuration settings for the application.
///
/// Includes settings for both the server and the message broker.
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub server: ServerSettings,
    pub broker: BrokerSettings,
}

/// Configuration settings for the server.
///
/// Defines the host and port the server will bind to.
#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
}

/// Configuration settings for the broker.
///
/// Controls operational parameters like maximum connections and message time-to-live.
#[derive(Debug, Deserialize, Clone)]
pub struct BrokerSettings {
    pub max_connections: usize,
    pub message_ttl_secs: u64,
}

/// Partial configuration settings loaded from files or environment.
///
/// Allows partial specification of settings. Missing values can be filled using defaults.
#[derive(Debug, Deserialize)]
pub struct PartialSettings {
    pub server: Option<PartialServerSettings>,
    pub broker: Option<PartialBrokerSettings>,
}

/// Partial server settings.
///
/// Used when loading server configuration from external sources with optional values.
#[derive(Debug, Deserialize)]
pub struct PartialServerSettings {
    pub host: Option<String>,
    pub port: Option<u16>,
}

/// Partial broker settings.
///
/// Used for broker configuration from external sources with optional values.
#[derive(Debug, Deserialize)]
pub struct PartialBrokerSettings {
    pub max_connections: Option<usize>,
    pub message_ttl_secs: Option<u64>,
}

/// Provides default values for `Settings`.
///
/// Ensures the application has sensible defaults if no configuration is provided.
impl Default for Settings {
    fn default() -> Self {
        Self {
            server: ServerSettings {
                host: "127.0.0.1".to_string(),
                port: 8080,
            },
            broker: BrokerSettings {
                max_connections: 1000,
                message_ttl_secs: 3600,
            },
        }
    }
}
