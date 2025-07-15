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
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrokerSettings {
    pub max_connections: usize,
    pub message_ttl_secs: u64,
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
}

#[derive(Debug, Deserialize)]
pub struct PartialBrokerSettings {
    pub max_connections: Option<usize>,
    pub message_ttl_secs: Option<u64>,
}

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
