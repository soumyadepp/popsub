//! Loads PopSub's server and broker settings.
//!
//! Values come from a TOML file, then environment variables, and anything
//! neither sets falls back to the setting types' defaults. The types live in
//! `settings`.

pub mod settings;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;

use config::{Config, ConfigError, Environment, File};

pub use settings::{
    BrokerSettings, LimitSettings, RetentionSettings, ServerSettings, Settings, StorageBackend,
    StorageSettings, TelemetrySettings, TopicRetentionSettings,
};

/// Where `load_config` looks for the config file, relative to the working
/// directory and without its `.toml` extension.
pub const DEFAULT_CONFIG_FILE: &str = "config/default";

/// Loads settings from `config/default.toml`, then the environment, then the defaults.
///
/// Environment variables name a section and a key. Single-word keys work
/// with one underscore (`SERVER_PORT`). Keys that contain an underscore need
/// a double underscore after the section (`SERVER__JWT_SECRET`,
/// `BROKER__MAX_RETRIES`), because a single underscore can't tell the
/// section apart from the key. Nested sections use a double underscore at
/// every level (`STORAGE__RETENTION__MAX_MESSAGES`). A list takes commas
/// (`SERVER__TRUSTED_PROXIES=10.0.0.1,10.1.0.0/16`). A missing file is fine.
///
/// Returns:
///     The merged settings.
///
/// Raises:
///     ConfigError::FileParse: When the file exists but isn't valid TOML.
///     ConfigError::Type: When a value has the wrong type.
pub fn load_config() -> Result<Settings, ConfigError> {
    load_config_from(Path::new(DEFAULT_CONFIG_FILE), None)
}

/// Loads settings as `load_config` does, from a chosen file and environment.
///
/// Tests use this to point at a temporary file and to pass variables without
/// touching the process environment or working directory.
///
/// Args:
///     file: The config file, with or without its `.toml` extension. It may
///         be missing.
///     env: Variables to read instead of the process environment, or `None`
///         to read the process environment.
///
/// Returns:
///     The merged settings.
///
/// Raises:
///     ConfigError::FileParse: When the file exists but isn't valid TOML.
///     ConfigError::Type: When a value has the wrong type.
pub fn load_config_from(
    file: &Path,
    env: Option<HashMap<String, String>>,
) -> Result<Settings, ConfigError> {
    Config::builder()
        .add_source(File::from(file).required(false))
        .add_source(Environment::default().separator("_").source(env.clone()))
        .add_source(Environment::default().separator("__").source(env))
        .build()?
        .try_deserialize()
}
