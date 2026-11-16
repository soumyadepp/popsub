//! Unit tests for settings defaults, the insecure-default checks and loading
//! from environment variables.

use std::path::Path;

use popsub_storage::Durability;

use crate::load_config_from;
use crate::settings::{LimitSettings, RetentionSettings, Settings, StorageBackend};

/// Checks the default host, port, connection cap and storage settings.
#[test]
fn test_default_settings() {
    let settings = Settings::default();
    assert_eq!(settings.server.host, "127.0.0.1");
    assert_eq!(settings.server.port, 8080);
    assert_eq!(settings.server.heartbeat_secs, 30);
    assert_eq!(settings.server.idle_timeout_secs, 90);
    assert_eq!(settings.server.max_payload_bytes, 1024 * 1024);
    assert_eq!(settings.server.outbound_queue_bytes, 16 * 1024 * 1024);
    assert_eq!(settings.broker.max_connections, 1000);
    assert_eq!(settings.storage.backend, StorageBackend::File);
    assert_eq!(settings.storage.path, "pubsub_data");
    assert_eq!(settings.storage.durability, Durability::Buffered);
    assert_eq!(settings.storage.retention.max_messages, 1000);
    assert_eq!(settings.storage.retention.max_age_secs, 3600);
    assert_eq!(settings.storage.session_expiry_secs, 86_400);
    assert_eq!(settings.broker.max_inflight, 100);
}

/// Checks that retention settings become a policy with every limit set and
/// the age converted to milliseconds.
#[test]
fn retention_settings_convert_to_a_policy() {
    let retention = RetentionSettings {
        max_messages: 5,
        max_age_secs: 2,
        max_bytes: 100,
    }
    .to_retention();
    assert_eq!(retention.max_messages, Some(5));
    assert_eq!(retention.max_age_ms, Some(2000));
    assert_eq!(retention.max_bytes, Some(100));
}

/// Checks that the nested storage settings load from the environment.
#[test]
fn storage_settings_load_from_the_environment() {
    let env = std::collections::HashMap::from([
        ("STORAGE__BACKEND".to_string(), "memory".to_string()),
        ("STORAGE__DURABILITY".to_string(), "none".to_string()),
        (
            "STORAGE__RETENTION__MAX_MESSAGES".to_string(),
            "50".to_string(),
        ),
    ]);
    let settings = load_config_from(Path::new("no/such/config"), Some(env)).expect("load failed");
    assert_eq!(settings.storage.retention.max_messages, 50);
    assert_eq!(settings.storage.retention.max_age_secs, 3600);
    assert_eq!(settings.storage.durability, Durability::None);
    assert_eq!(settings.storage.backend, StorageBackend::Memory);
}

/// Checks that an unknown backend or durability name is an error.
#[test]
fn unknown_storage_names_are_errors() {
    for (key, value) in [
        ("STORAGE__BACKEND", "redis"),
        ("STORAGE__DURABILITY", "sometimes"),
    ] {
        let env = std::collections::HashMap::from([(key.to_string(), value.to_string())]);
        assert!(
            load_config_from(Path::new("no/such/config"), Some(env)).is_err(),
            "{key}={value}"
        );
    }
}

/// Checks that the defaults report both published values, bind to loopback and keep registration off.
#[test]
fn shipped_defaults_are_reported() {
    let settings = Settings::default();
    assert_eq!(settings.insecure_defaults().len(), 2);
    assert!(settings.server.is_loopback());
    assert!(!settings.server.allow_registration);
}

/// Checks that changed secrets clear the warnings and that only loopback hosts count as loopback.
#[test]
fn changed_secrets_pass_and_public_hosts_are_detected() {
    let mut settings = Settings::default();
    settings.server.jwt_secret = "a-long-random-secret-from-a-vault".to_string();
    settings.server.password = "correct horse battery staple".to_string();
    assert!(settings.insecure_defaults().is_empty());

    for host in ["0.0.0.0", "192.168.1.10", "example.com"] {
        settings.server.host = host.to_string();
        assert!(!settings.server.is_loopback(), "{host}");
    }
    for host in ["localhost", "127.0.0.1", "::1"] {
        settings.server.host = host.to_string();
        assert!(settings.server.is_loopback(), "{host}");
    }
}

/// Checks that double-underscore variables set multi-word keys while `SERVER_PORT` still works with one.
#[test]
fn multi_word_keys_load_from_double_underscore_variables() {
    let env = std::collections::HashMap::from([
        (
            "SERVER__JWT_SECRET".to_string(),
            "from-the-environment".to_string(),
        ),
        ("BROKER__MAX_RETRIES".to_string(), "9".to_string()),
        ("SERVER__ALLOW_REGISTRATION".to_string(), "true".to_string()),
        ("SERVER_PORT".to_string(), "9100".to_string()),
    ]);
    let settings = load_config_from(Path::new("no/such/config"), Some(env)).expect("load failed");

    assert_eq!(settings.server.jwt_secret, "from-the-environment");
    assert_eq!(settings.broker.max_retries, 9);
    assert!(settings.server.allow_registration);
    assert_eq!(settings.server.port, 9100);
}

/// Checks that a config file overrides only the values it names and the rest keep their defaults.
#[test]
fn a_partial_file_overrides_only_what_it_names() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let file = dir.path().join("popsub.toml");
    std::fs::write(
        &file,
        r#"
            [server]
            host = "0.0.0.0"
            jwt_secret = "file_secret"

            [storage.retention]
            max_age_secs = 60
        "#,
    )
    .expect("write config file");

    let settings = load_config_from(&file, Some(Default::default())).expect("load failed");
    let defaults = Settings::default();

    assert_eq!(settings.server.host, "0.0.0.0");
    assert_eq!(settings.server.jwt_secret, "file_secret");
    assert_eq!(settings.storage.retention.max_age_secs, 60);
    assert_eq!(
        settings.storage.retention.max_messages,
        defaults.storage.retention.max_messages
    );
    assert_eq!(settings.server.port, defaults.server.port);
    assert_eq!(settings.server.username, defaults.server.username);
    assert_eq!(settings.broker.max_retries, defaults.broker.max_retries);
}

/// Checks that environment variables win over the config file.
#[test]
fn environment_overrides_the_file() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let file = dir.path().join("popsub.toml");
    std::fs::write(&file, "[server]\nport = 9000\n").expect("write config file");
    let env = std::collections::HashMap::from([("SERVER_PORT".to_string(), "9200".to_string())]);

    let settings = load_config_from(&file, Some(env)).expect("load failed");

    assert_eq!(settings.server.port, 9200);
}

/// Checks that a value of the wrong type is reported instead of silently replaced by the default.
#[test]
fn wrong_types_are_errors() {
    let env =
        std::collections::HashMap::from([("SERVER_PORT".to_string(), "not-a-port".to_string())]);
    assert!(load_config_from(Path::new("no/such/config"), Some(env)).is_err());
}

/// Checks that the rate limits and trusted proxies load from the environment,
/// with commas splitting the proxy list, and default to today's limits and no proxies.
#[test]
fn limits_and_trusted_proxies_load_from_the_environment() {
    let defaults = load_config_from(Path::new("no/such/config"), Some(Default::default()))
        .expect("load failed");
    assert_eq!(defaults.limits, LimitSettings::default());
    assert!(defaults.server.trusted_proxies.is_empty());

    let env = std::collections::HashMap::from([
        ("LIMITS__LOGIN_ATTEMPTS".to_string(), "3".to_string()),
        ("LIMITS__PUBLISH_PER_SECOND".to_string(), "10".to_string()),
        (
            "SERVER__TRUSTED_PROXIES".to_string(),
            "10.0.0.1,192.168.0.0/16".to_string(),
        ),
        ("BROKER__MAX_CONNECTIONS".to_string(), "20".to_string()),
    ]);
    let settings = load_config_from(Path::new("no/such/config"), Some(env)).expect("load failed");
    assert_eq!(settings.limits.login_attempts, 3);
    assert_eq!(settings.limits.publish_per_second, 10);
    assert_eq!(settings.limits.publish_burst, 50);
    assert_eq!(
        settings.server.trusted_proxies,
        vec!["10.0.0.1".to_string(), "192.168.0.0/16".to_string()]
    );
    assert_eq!(settings.broker.max_connections, 20);
}

/// Checks that the `[limits]` section and a proxy list load from a file.
#[test]
fn limits_and_trusted_proxies_load_from_a_file() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let file = dir.path().join("popsub.toml");
    std::fs::write(
        &file,
        "[server]\ntrusted_proxies = [\"::1\", \"10.0.0.0/8\"]\n\n[limits]\nregistrations_per_hour = 2\n",
    )
    .expect("write config");
    let settings = load_config_from(&file, Some(Default::default())).expect("load failed");
    assert_eq!(settings.server.trusted_proxies, vec!["::1", "10.0.0.0/8"]);
    assert_eq!(settings.limits.registrations_per_hour, 2);
    assert_eq!(settings.limits.login_attempts, 5);
}
