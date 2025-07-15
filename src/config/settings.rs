use serde::Deserialize;

/// Represents the configuration settings for the application
/// This struct contains settings for the server and broker,
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub server: ServerSettings,
    pub broker: BrokerSettings,
}

/// Represents the server settings for the application
/// Contains the host and port on which the server will run.
#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
}
/// Represents the broker settings for the application
/// Contains settings related to the broker's operation, such as maximum connections and message TTL.
/// These settings are used to configure the broker's behavior and resource limits.
#[derive(Debug, Deserialize, Clone)]
pub struct BrokerSettings {
    pub max_connections: usize,
    pub message_ttl_secs: u64,
}

/// Represents partial settings that can be loaded from configuration files or environment variables
/// This struct allows for flexible configuration where only certain fields may be specified.
/// It is used to load settings that may not be fully defined, allowing defaults to be applied
/// when necessary.
#[derive(Debug, Deserialize)]
pub struct PartialSettings {
    pub server: Option<PartialServerSettings>,
    pub broker: Option<PartialBrokerSettings>,
}

/// Represents partial server settings that can be loaded from configuration files or environment variables
/// This struct allows for flexible configuration of the server's host and port.
#[derive(Debug, Deserialize)]
pub struct PartialServerSettings {
    pub host: Option<String>,
    pub port: Option<u16>,
}

/// Represents partial broker settings that can be loaded from configuration files or environment variables
/// This struct allows for flexible configuration of the broker's maximum connections and message TTL.
/// It is used to load settings that may not be fully defined, allowing defaults to be applied
/// when necessary.
#[derive(Debug, Deserialize)]
pub struct PartialBrokerSettings {
    pub max_connections: Option<usize>,
    pub message_ttl_secs: Option<u64>,
}

/// Default implementation for `Settings`
/// Provides default values for server and broker settings.
/// This implementation ensures that if no configuration is provided, the application will still have sensible defaults.
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
