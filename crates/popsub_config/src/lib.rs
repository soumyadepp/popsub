pub mod settings;
pub mod tests;

use crate::settings::PartialSettings;
use config::{Config, ConfigError, Environment, File};

pub use settings::Settings;

pub use settings::{BrokerSettings, ServerSettings};

pub fn load_config() -> Result<Settings, ConfigError> {
    let builder = Config::builder()
        .add_source(File::with_name("config/default").required(false))
        .add_source(Environment::default().separator("_"));

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
mod env_tests {
    use super::*;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn load_config_from_file_overrides_defaults() {
        // Create a temporary directory and set it as current dir so load_config
        // will pick up config/default.toml from there.
        let tmp = TempDir::new().expect("create tempdir");
        let orig = env::current_dir().expect("current_dir");
        env::set_current_dir(tmp.path()).expect("set current dir");

        // create config dir and default.toml
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

        // restore cwd
        env::set_current_dir(orig).expect("restore cwd");
    }
}
