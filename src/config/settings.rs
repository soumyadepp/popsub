use serde::Deserialize;

/// Top-level configuration settings for the application.
///
/// This struct aggregates all configuration parameters required to run the PopSub server,
/// including network settings for the server and operational parameters for the broker.
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    /// Configuration specific to the WebSocket server, such as host and port.
    pub server: ServerSettings,
    /// Configuration specific to the message broker, such as connection limits and message TTL.
    pub broker: BrokerSettings,
}

/// Configuration settings for the server.
///
/// Defines the network parameters for the WebSocket server, including the address
/// it will bind to and the port it will listen on.
#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    /// The host address (e.g., "127.0.0.1" or "0.0.0.0") the server will bind to.
    pub host: String,
    /// The port number the server will listen on.
    pub port: u16,
}

/// Configuration settings for the broker.
///
/// Controls operational parameters for the message broker, such as limits on
/// concurrent connections and the time-to-live for messages in the persistence layer.
#[derive(Debug, Deserialize, Clone)]
pub struct BrokerSettings {
    /// The maximum number of concurrent client connections the broker will accept.
    pub max_connections: usize,
    /// The time-to-live (in seconds) for messages stored in the persistence layer.
    /// Messages older than this duration may be purged.
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
