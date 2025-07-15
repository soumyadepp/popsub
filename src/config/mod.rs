mod settings;

use config::{Config, ConfigError, Environment, File};
use settings::Settings;

pub fn load_config() -> Result<Settings, ConfigError> {
    let builder = Config::builder()
        .add_source(File::with_name("config/default").required(false)) // config/default.toml
        .add_source(config::Environment::default().separator("_"));    // e.g., SERVER_PORT

    let config = builder.build()?;
    config.try_deserialize()
}
