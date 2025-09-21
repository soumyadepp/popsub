use super::settings::Settings;

#[test]
fn test_default_settings() {
    let settings = Settings::default();
    assert_eq!(settings.server.host, "127.0.0.1");
    assert_eq!(settings.server.port, 8080);
    assert_eq!(settings.broker.max_connections, 1000);
    assert_eq!(settings.broker.message_ttl_secs, 3600);
}
