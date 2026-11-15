//! Loads PopSub's server and broker settings.
//!
//! Values come from `config/default.toml` (relative to the working
//! directory), then environment variables, then `Settings::default()` for
//! anything neither source set. The setting types live in `settings`.

pub mod settings;
pub mod tests;

use crate::settings::PartialSettings;
use config::{Config, ConfigError, Environment, File};

pub use settings::Settings;

pub use settings::{BrokerSettings, ServerSettings};

/// Loads settings from `config/default.toml`, then the environment, then
/// built-in defaults for anything still missing.
///
/// Environment variables name a section and a key. Single-word keys work
/// with one underscore (`SERVER_PORT`). Keys that contain an underscore
/// need a double underscore after the section (`SERVER__JWT_SECRET`,
/// `BROKER__MAX_RETRIES`), because a single underscore can't tell the
/// section apart from the key.
///
/// Returns:
///     The merged settings.
///
/// Raises:
///     ConfigError::FileParse: When `config/default.toml` exists but can't be parsed.
///     ConfigError::Type: When a value from the file or the environment has the wrong type.
pub fn load_config() -> Result<Settings, ConfigError> {
    load_config_with_env(None)
}

/// Loads settings as `load_config` does, reading environment variables from
/// `env` instead of the process when given. Used by tests.
///
/// The environment is added twice, once split on `_` and once on `__`, so
/// both `SERVER_PORT` and `SERVER__JWT_SECRET` reach the right key. A
/// multi-word key written with single underscores (`SERVER_JWT_SECRET`)
/// becomes a nested key that no field matches, and is ignored.
///
/// Args:
///     env: Variables to use in place of the process environment, or `None`
///         to read the process environment.
///
/// Returns:
///     The merged settings.
///
/// Raises:
///     ConfigError::FileParse: When `config/default.toml` exists but can't be parsed.
///     ConfigError::Type: When a value from the file or the environment has the wrong type.
pub fn load_config_with_env(
    env: Option<std::collections::HashMap<String, String>>,
) -> Result<Settings, ConfigError> {
    let builder = Config::builder()
        .add_source(File::with_name("config/default").required(false))
        .add_source(Environment::default().separator("_").source(env.clone()))
        .add_source(Environment::default().separator("__").source(env));

    let config = builder.build()?;
    let partial: PartialSettings = config.try_deserialize()?;
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
            jwt_expiration_hours: partial
                .server
                .as_ref()
                .and_then(|s| s.jwt_expiration_hours)
                .unwrap_or(default.server.jwt_expiration_hours),
            username: partial
                .server
                .as_ref()
                .and_then(|s| s.username.clone())
                .unwrap_or(default.server.username),
            password: partial
                .server
                .as_ref()
                .and_then(|s| s.password.clone())
                .unwrap_or(default.server.password),
            allow_registration: partial
                .server
                .as_ref()
                .and_then(|s| s.allow_registration)
                .unwrap_or(default.server.allow_registration),
            registration_role: partial
                .server
                .as_ref()
                .and_then(|s| s.registration_role.clone())
                .unwrap_or(default.server.registration_role),
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
            ack_timeout_ms: partial
                .broker
                .as_ref()
                .and_then(|b| b.ack_timeout_ms)
                .unwrap_or(default.broker.ack_timeout_ms),
            max_retries: partial
                .broker
                .as_ref()
                .and_then(|b| b.max_retries)
                .unwrap_or(default.broker.max_retries),
        },
    })
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    /// Checks that values in `config/default.toml`, read from a temporary working directory, override the defaults.
    #[test]
    fn load_config_from_file_overrides_defaults() {
        let tmp = TempDir::new().expect("create tempdir");
        let orig = env::current_dir().expect("current_dir");
        env::set_current_dir(tmp.path()).expect("set current dir");

        fs::create_dir_all("config").expect("create config dir");
        let toml = r#"
            [server]
            host = "0.0.0.0"
            port = 9000
            jwt_secret = "file_secret"

            [broker]
            max_connections = 10
            message_ttl_secs = 60
        "#;
        fs::write("config/default.toml", toml).expect("write config file");

        let cfg = load_config().expect("load_config failed");
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.port, 9000);
        assert_eq!(cfg.server.jwt_secret, "file_secret");
        assert_eq!(cfg.broker.max_connections, 10);
        assert_eq!(cfg.broker.message_ttl_secs, 60);

        env::set_current_dir(orig).expect("restore cwd");
    }
}
