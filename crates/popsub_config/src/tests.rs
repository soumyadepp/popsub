pub use super::settings::Settings;

#[test]
fn test_default_settings() {
    let settings = Settings::default();
    assert_eq!(settings.server.host, "127.0.0.1");
    assert_eq!(settings.server.port, 8080);
    assert_eq!(settings.broker.max_connections, 1000);
    assert_eq!(settings.broker.message_ttl_secs, 3600);
}

#[test]
fn shipped_defaults_are_reported() {
    let settings = Settings::default();
    assert_eq!(settings.insecure_defaults().len(), 2);
    assert!(settings.server.is_loopback());
    assert!(!settings.server.allow_registration);
}

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
