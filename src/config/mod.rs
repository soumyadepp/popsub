mod settings;

use crate::config::settings::PartialSettings;
use config::{Config, ConfigError, Environment, File};

use settings::Settings;

pub use settings::{BrokerSettings, ServerSettings};

/// Loads the configuration from the default file and environment variables
/// Merges the configuration with default values
/// Returns a `Settings` struct containing the server and broker configurations
pub fn load_config() -> Result<Settings, ConfigError> {
    let builder = Config::builder()
        .add_source(File::with_name("config/default").required(false))
        .add_source(Environment::default().separator("_"));

    let config = builder.build()?;

    // Try to deserialize what is available
    let partial: PartialSettings = config.try_deserialize()?;

    // Merge with defaults
    let default = Settings::default();

    Ok(Settings {
        server: ServerSettings {
            host: partial
                .server
                .as_ref()
                .and_then(|s| s.host.clone())
                .unwrap_or(default.server.host),
            port: partial
                .server
                .as_ref()
                .and_then(|s| s.port)
                .unwrap_or(default.server.port),
        },
        broker: BrokerSettings {
            max_connections: partial
                .broker
                .as_ref()
                .and_then(|b| b.max_connections)
                .unwrap_or(default.broker.max_connections),
            message_ttl_secs: partial
                .broker
                .as_ref()
                .and_then(|b| b.message_ttl_secs)
                .unwrap_or(default.broker.message_ttl_secs),
        },
    })
}
