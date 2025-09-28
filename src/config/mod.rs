//! The `config` module handles the application's configuration management.
//!
//! It defines the structure of the application settings, provides default values,
//! and implements the logic for loading configuration from various sources,
//! such as configuration files and environment variables.
//!
//! This module uses the `config` crate for flexible and layered configuration.

pub mod settings;

use crate::config::settings::PartialSettings;
use config::{Config, ConfigError, Environment, File};

pub use settings::Settings;

pub use settings::{BrokerSettings, ServerSettings};

/// Loads application settings from file, environment, and defaults.
///
/// This function reads configuration values from:
/// 1. An optional `config/default` file (TOML/YAML/JSON).
/// 2. Environment variables with `_` separators (e.g., `SERVER_PORT`).
///
/// Missing fields are filled using default values defined in `Settings::default()`.
///
/// # Returns
///
/// A fully resolved [`Settings`] struct, or a [`ConfigError`] if deserialization fails.
///
/// # Environment Variable Examples
///
/// - `SERVER_HOST=0.0.0.0`
/// - `BROKER_MAX_CONNECTIONS=2000`
///
/// # Errors
///
/// Returns an error if the config file cannot be parsed or deserialized.
///
/// # Example
///
/// ```rust
/// use popsub::config::load_config;
/// let settings = load_config().expect("Failed to load config");
/// println!("Running on {}:{}", settings.server.host, settings.server.port);
/// ```
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
            jwt_secret: partial
                .server
                .as_ref()
                .and_then(|s| s.jwt_secret.clone())
                .unwrap_or(default.server.jwt_secret),
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

#[cfg(test)]
mod tests;
