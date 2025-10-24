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
}

#[derive(Debug, Deserialize)]
pub struct PartialBrokerSettings {
    pub max_connections: Option<usize>,
    pub message_ttl_secs: Option<u64>,
    pub ack_timeout_ms: Option<u64>,
    pub max_retries: Option<u8>,
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
