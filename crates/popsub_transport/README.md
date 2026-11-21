# popsub_transport

WebSocket transport implementation using `tokio-tungstenite`.

Build a `Server` with `Server::from_settings(broker, &settings.server)` (users in memory) or `Server::from_settings_with_users(broker, &settings.server, users)` (any `UserStore`, such as `FileUserStore`), then run it with `listen(addr)` or `serve(listener)`. It checks logins, topic permissions and rate limits before calling the broker.
