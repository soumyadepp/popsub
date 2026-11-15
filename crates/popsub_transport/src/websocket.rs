//! WebSocket transport
//!
//! This file implements a minimal WebSocket server that translates protocol
//! JSON messages into broker operations. Responsibilities:
//! - Accept TCP/WebSocket connections
//! - Create a `Client` for each connection and register it with the `Broker`
//! - Enforce a login -> auth -> other-message order: clients must authenticate
//!   before subscribing or publishing
//! - Perform authorization checks using `AuthService` for subscribe/publish
//! - Serialize/deserialize JSON messages and forward them to the broker
//!
//! Security note: Authentication is handled via JWT tokens managed by `AuthService`.
//! Authorization is enforced per-topic based on user roles and permissions.

use futures_util::{SinkExt, StreamExt};
use popsub_auth::{AuthError, AuthService};
use popsub_ratelimit::{LoginRateLimiter, RateLimiter, SlidingWindow, TokenBucket};
use popsub_utils::error::{PopSubError, Result};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{error, info, warn};

use crate::message::{ClientMessage, ServerMessage};
use popsub_broker::engine::Broker;
use popsub_client::Client;

/// Rate limiter configuration for the WebSocket server.
#[derive(Clone)]
pub struct RateLimiters {
    /// Failed logins per address and username pair. Keying by the pair means
    /// someone guessing passwords for a user locks out only themselves, not
    /// the real user connecting from elsewhere.
    pub login_limiter: Arc<LoginRateLimiter>,
    /// Failed logins per address across all usernames, to slow down guessing
    /// spread over many accounts.
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    /// Registration attempts per address. Every attempt counts, so
    /// reconnecting doesn't reset the allowance.
    pub register_limiter: Arc<SlidingWindow>,
    /// Published messages per authenticated user, shared by all of that
    /// user's connections.
    pub message_limiter: Arc<TokenBucket>,
}

/// How often idle rate limiter entries are pruned.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);
/// Login limiter entries are kept at least as long as the longest lockout.
const LOGIN_IDLE: Duration = Duration::from_secs(3600);
/// Message buckets refill fully well within this time.
const MESSAGE_IDLE: Duration = Duration::from_secs(600);

impl RateLimiters {
    /// Remove entries for keys that have been quiet long enough to forget.
    ///
    /// Returns:
    ///   How many entries were removed in total.
    pub fn prune_idle(&self) -> usize {
        self.login_limiter.prune_idle(LOGIN_IDLE)
            + self.ip_login_limiter.prune_idle(LOGIN_IDLE)
            + self.register_limiter.prune_idle()
            + self.message_limiter.prune_idle(MESSAGE_IDLE)
    }
}

impl Default for RateLimiters {
    fn default() -> Self {
        Self {
            // 5 failed attempts, 5 minute lockout, exponential backoff up to 1 hour
            login_limiter: Arc::new(
                LoginRateLimiter::builder()
                    .max_attempts(5)
                    .lockout_duration(Duration::from_secs(300))
                    .exponential_backoff(true)
                    .max_lockout_duration(Duration::from_secs(3600))
                    .build(),
            ),
            // 20 failed logins from one address, across any usernames
            ip_login_limiter: Arc::new(
                LoginRateLimiter::builder()
                    .max_attempts(20)
                    .lockout_duration(Duration::from_secs(900))
                    .exponential_backoff(true)
                    .max_lockout_duration(Duration::from_secs(3600))
                    .build(),
            ),
            // 5 registration attempts per address per hour
            register_limiter: Arc::new(SlidingWindow::new(5, Duration::from_secs(3600))),
            // 100 messages per second, burst of 50
            message_limiter: Arc::new(TokenBucket::new(100, 50, Duration::from_secs(1))),
        }
    }
}

/// Start the WebSocket server with authentication support.
///
/// # Arguments
///
/// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
/// * `broker` - The shared broker instance
/// * `auth_service` - The authentication service for login/auth/authorization
pub async fn start_websocket_server_with_auth(
    addr: String,
    broker: Arc<Mutex<Broker>>,
    auth_service: Arc<RwLock<AuthService>>,
) {
    start_websocket_server_with_rate_limiting(addr, broker, auth_service, RateLimiters::default())
        .await
}

/// Start the WebSocket server with authentication and custom rate limiting.
///
/// # Arguments
///
/// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
/// * `broker` - The shared broker instance
/// * `auth_service` - The authentication service for login/auth/authorization
/// * `rate_limiters` - Custom rate limiter configuration
pub async fn start_websocket_server_with_rate_limiting(
    addr: String,
    broker: Arc<Mutex<Broker>>,
    auth_service: Arc<RwLock<AuthService>>,
    rate_limiters: RateLimiters,
) {
    let listener = TcpListener::bind(addr.clone()).await.expect("Can't bind");

    info!("WebSocket server listening on ws://{}", addr);

    {
        let rate_limiters = rate_limiters.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
            loop {
                ticker.tick().await;
                let pruned = rate_limiters.prune_idle();
                if pruned > 0 {
                    info!(pruned, "pruned idle rate limiter entries");
                }
            }
        });
    }

    while let Ok((stream, peer)) = listener.accept().await {
        let peer_ip = peer.ip().to_string();
        let broker = broker.clone();
        let auth_service = auth_service.clone();
        let rate_limiters = rate_limiters.clone();

        tokio::spawn(async move {
            let ws_stream = match accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("WebSocket handshake error: {e}");
                    return;
                }
            };

            let (ws_sender, mut ws_receiver) = ws_stream.split();
            let (tx, rx) = mpsc::unbounded_channel::<WsMessage>();
            let client = Client::new(tx.clone());
            let client_id = client.id.clone();

            {
                let mut broker = broker.lock().expect("Broker lock poisoned");
                broker.register_client(client);
            }

            let cleanup_called = Arc::new(AtomicBool::new(false));

            let do_cleanup = {
                let broker = broker.clone();
                let client_id = client_id.clone();
                let cleanup_called = cleanup_called.clone();

                move || {
                    if !cleanup_called.swap(true, Ordering::SeqCst) {
                        let mut broker = broker.lock().expect("Broker lock poisoned");
                        broker.cleanup_client(&client_id);
                    }
                }
            };

            spawn_send_loop(ws_sender, rx, client_id.clone(), do_cleanup.clone());

            while let Some(Ok(msg)) = ws_receiver.next().await {
                if msg.is_text() {
                    let text = match msg.to_text() {
                        Ok(text) => text,
                        Err(_) => {
                            warn!(client_id = %client_id, "Received non-text message");
                            continue;
                        }
                    };

                    // Parse before taking any locks
                    let parsed = match serde_json::from_str::<ClientMessage>(text) {
                        Ok(pm) => pm,
                        Err(err) => {
                            error!(
                                client_id = %client_id,
                                "Invalid client message: {} | {}",
                                err,
                                &text.chars().take(100).collect::<String>()
                            );
                            continue;
                        }
                    };

                    // Handle the message
                    let result = handle_message(
                        &broker,
                        &auth_service,
                        &rate_limiters,
                        &client_id,
                        &peer_ip,
                        parsed,
                    )
                    .await;

                    match result {
                        Ok(should_continue) => {
                            if !should_continue {
                                break;
                            }
                        }
                        Err(e) => {
                            error!(client_id = %client_id, "Error handling message: {}", e);
                            let broker_lock = broker.lock().expect("Broker lock poisoned");
                            if let Some(client) = broker_lock.clients.get(&client_id) {
                                let response = ServerMessage::Error {
                                    message: "internal server error".to_string(),
                                };
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = client.sender.send(WsMessage::Text(json.into()));
                                }
                            }
                            break;
                        }
                    }
                }
            }

            do_cleanup();
        });
    }
}

/// Handle a parsed client message with authentication and authorization.
///
/// Returns `Ok(true)` to continue processing, `Ok(false)` to disconnect the client.
async fn handle_message(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    rate_limiters: &RateLimiters,
    client_id: &str,
    peer_ip: &str,
    msg: ClientMessage,
) -> Result<bool> {
    match msg {
        ClientMessage::Login { username, password } => {
            handle_login(
                broker,
                auth_service,
                rate_limiters,
                client_id,
                peer_ip,
                &username,
                &password,
            )
            .await
        }
        ClientMessage::Register { username, password } => {
            handle_register(
                broker,
                auth_service,
                rate_limiters,
                client_id,
                peer_ip,
                &username,
                &password,
            )
            .await
        }
        ClientMessage::Auth { token } => handle_auth(broker, auth_service, client_id, &token).await,
        ClientMessage::Subscribe { topic } => {
            handle_subscribe(broker, auth_service, client_id, &topic).await
        }
        ClientMessage::Unsubscribe { topic } => handle_unsubscribe(broker, client_id, &topic),
        ClientMessage::Publish {
            topic,
            payload,
            message_id,
            qos,
        } => {
            handle_publish(
                broker,
                auth_service,
                rate_limiters,
                client_id,
                &topic,
                payload,
                message_id,
                qos,
            )
            .await
        }
        ClientMessage::Ack { message_id } => handle_ack(broker, client_id, &message_id),
    }
}

/// Handle registration request - create a new user account.
#[allow(clippy::too_many_arguments)]
async fn handle_register(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    rate_limiters: &RateLimiters,
    client_id: &str,
    peer_ip: &str,
    username: &str,
    password: &str,
) -> Result<bool> {
    if !rate_limiters.register_limiter.try_acquire(peer_ip).await {
        let retry_after = rate_limiters
            .register_limiter
            .retry_after(peer_ip)
            .await
            .unwrap_or(1);
        warn!(client_id = %client_id, peer_ip = %peer_ip, retry_after, "registration rate limited");
        let response = ServerMessage::RegisterResponse {
            success: false,
            message: format!("Too many attempts. Try again in {retry_after} seconds."),
        };
        send_to_client(broker, client_id, response)?;
        return Ok(true);
    }

    let auth = auth_service.read().await;

    let response = match auth.register_user(username, password).await {
        Ok(()) => {
            info!(client_id = %client_id, username = %username, "registration successful");
            ServerMessage::RegisterResponse {
                success: true,
                message: "User registered successfully".to_string(),
            }
        }
        Err(e) => {
            warn!(client_id = %client_id, username = %username, "registration failed: {}", e);
            ServerMessage::RegisterResponse {
                success: false,
                message: registration_error_message(&e),
            }
        }
    };

    send_to_client(broker, client_id, response)?;
    Ok(true)
}

/// Turns a registration error into text that is safe to show the client.
///
/// Validation messages are shown as they are, because they help a genuine
/// user fix their input. Whether a username is already taken is not
/// revealed, so the endpoint can't be used to list existing accounts.
///
/// Args:
///   error: The error from the auth service.
///
/// Returns:
///   The message to send back.
fn registration_error_message(error: &AuthError) -> String {
    match error {
        AuthError::PermissionDenied(reason) => reason.clone(),
        _ => "registration failed".to_string(),
    }
}

/// Handle login request - authenticate user and return JWT token.
#[allow(clippy::too_many_arguments)]
async fn handle_login(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    rate_limiters: &RateLimiters,
    client_id: &str,
    peer_ip: &str,
    username: &str,
    password: &str,
) -> Result<bool> {
    let pair_key = format!("{peer_ip}|{username}");
    let blocked = match rate_limiters.ip_login_limiter.check_status(peer_ip).await {
        popsub_ratelimit::LoginStatus::LockedOut { retry_after } => Some(retry_after),
        _ => match rate_limiters.login_limiter.check_status(&pair_key).await {
            popsub_ratelimit::LoginStatus::LockedOut { retry_after } => Some(retry_after),
            _ => None,
        },
    };
    if let Some(retry_after) = blocked {
        warn!(
            client_id = %client_id,
            peer_ip = %peer_ip,
            username = %username,
            retry_after = %retry_after,
            "login rate limited"
        );
        let response = ServerMessage::Error {
            message: format!("Too many login attempts. Try again in {retry_after} seconds."),
        };
        send_to_client(broker, client_id, response)?;
        return Ok(true);
    }

    let auth = auth_service.read().await;

    let response = match auth.login(username, password).await {
        Ok(token) => {
            info!(client_id = %client_id, username = %username, "login successful");
            rate_limiters.login_limiter.record_success(&pair_key).await;
            ServerMessage::LoginResponse { token }
        }
        Err(e) => {
            warn!(client_id = %client_id, username = %username, "login failed: {}", e);
            if matches!(e, AuthError::InvalidCredentials) {
                rate_limiters.login_limiter.record_failure(&pair_key).await;
                rate_limiters.ip_login_limiter.record_failure(peer_ip).await;
            }
            ServerMessage::Error {
                message: "invalid credentials".to_string(),
            }
        }
    };

    send_to_client(broker, client_id, response)?;
    Ok(true)
}

/// Handle auth request - validate JWT token and mark client as authenticated.
async fn handle_auth(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    token: &str,
) -> Result<bool> {
    let auth = auth_service.read().await;

    match auth.validate_token(token) {
        Ok(claims) => {
            let username = claims.sub.clone();
            drop(auth); // Release read lock before acquiring broker lock

            let mut broker_lock = broker.lock().expect("Broker lock poisoned");
            if let Some(client) = broker_lock.clients.get_mut(client_id) {
                client.authenticated = true;
                client.username = Some(username.clone());
                info!(client_id = %client_id, username = %username, "authenticated successfully");

                let response = ServerMessage::Authenticated {};
                let json = serde_json::to_string(&response)?;
                let _ = client.sender.send(WsMessage::Text(json.into()));
            }
            Ok(true)
        }
        Err(e) => {
            warn!(client_id = %client_id, "authentication failed: {}", e);
            let response = ServerMessage::Error {
                message: "authentication failed".to_string(),
            };
            send_to_client(broker, client_id, response)?;
            Ok(false)
        }
    }
}

/// Handle subscribe request with authorization check.
async fn handle_subscribe(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    topic: &str,
) -> Result<bool> {
    // First check authentication and get username
    let (username, sender) = {
        let broker_lock = broker.lock().expect("Broker lock poisoned");

        let client = match broker_lock.clients.get(client_id) {
            Some(c) => c,
            None => {
                warn!(client_id = %client_id, "Client not found");
                return Ok(false);
            }
        };

        // Check authentication
        if !client.authenticated {
            warn!(client_id = %client_id, "subscribe attempt before authentication");
            let response = ServerMessage::Error {
                message: "must authenticate first".to_string(),
            };
            let json = serde_json::to_string(&response)?;
            let _ = client.sender.send(WsMessage::Text(json.into()));
            return Ok(false);
        }

        (
            client.username.clone().unwrap_or_default(),
            client.sender.clone(),
        )
    }; // broker_lock dropped here

    // Check authorization (async)
    let auth = auth_service.read().await;
    if !auth.can_subscribe(&username, topic).await {
        warn!(client_id = %client_id, username = %username, topic = %topic, "subscribe denied");
        let response = ServerMessage::Error {
            message: format!("not authorized to subscribe to '{topic}'"),
        };
        drop(auth);
        let json = serde_json::to_string(&response)?;
        let _ = sender.send(WsMessage::Text(json.into()));
        return Ok(true); // Don't disconnect, just deny this request
    }
    drop(auth);

    // Re-acquire broker lock for subscription
    let mut broker_lock = broker.lock().expect("Broker lock poisoned");
    broker_lock.subscribe(topic, client_id.to_string());
    info!(client_id = %client_id, username = %username, topic = %topic, "subscribed");

    Ok(true)
}

/// Handle unsubscribe request.
fn handle_unsubscribe(broker: &Arc<Mutex<Broker>>, client_id: &str, topic: &str) -> Result<bool> {
    let mut broker_lock = broker.lock().expect("Broker lock poisoned");

    let client = match broker_lock.clients.get(client_id) {
        Some(c) => c,
        None => {
            warn!(client_id = %client_id, "Client not found");
            return Ok(false);
        }
    };

    // Check authentication
    if !client.authenticated {
        warn!(client_id = %client_id, "unsubscribe attempt before authentication");
        let response = ServerMessage::Error {
            message: "must authenticate first".to_string(),
        };
        let json = serde_json::to_string(&response)?;
        let _ = client.sender.send(WsMessage::Text(json.into()));
        return Ok(false);
    }

    let username = client.username.clone().unwrap_or_default();

    broker_lock.unsubscribe(topic, &client_id.to_string());
    info!(client_id = %client_id, username = %username, topic = %topic, "unsubscribed");

    Ok(true)
}

/// Handle publish request with authorization check and rate limiting.
#[allow(clippy::too_many_arguments)]
async fn handle_publish(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    rate_limiters: &RateLimiters,
    client_id: &str,
    topic: &str,
    payload: String,
    message_id: Option<String>,
    qos: Option<u8>,
) -> Result<bool> {
    // First check authentication and get username
    // Authentication is checked before rate limiting to prevent unauthenticated
    // clients from consuming rate limit tokens of legitimate users.
    let (username, sender) = {
        let broker_lock = broker.lock().expect("Broker lock poisoned");

        let client = match broker_lock.clients.get(client_id) {
            Some(c) => c,
            None => {
                warn!(client_id = %client_id, "Client not found");
                return Ok(false);
            }
        };

        // Check authentication
        if !client.authenticated {
            warn!(client_id = %client_id, "publish attempt before authentication");
            let response = ServerMessage::Error {
                message: "must authenticate first".to_string(),
            };
            let json = serde_json::to_string(&response)?;
            let _ = client.sender.send(WsMessage::Text(json.into()));
            return Ok(false);
        }

        (
            client.username.clone().unwrap_or_default(),
            client.sender.clone(),
        )
    }; // broker_lock dropped here

    // Check message rate limit after authentication
    // This ensures only authenticated clients consume rate limit tokens.
    if !rate_limiters.message_limiter.try_acquire(&username).await {
        warn!(client_id = %client_id, "message rate limited");
        let response = ServerMessage::Error {
            message: "Rate limited. Too many messages.".to_string(),
        };
        send_to_client(broker, client_id, response)?;
        return Ok(true); // Don't disconnect, just rate limit
    }

    // Check authorization (async)
    let auth = auth_service.read().await;
    if !auth.can_publish(&username, topic).await {
        warn!(client_id = %client_id, username = %username, topic = %topic, "publish denied");
        let response = ServerMessage::Error {
            message: format!("not authorized to publish to '{topic}'"),
        };
        drop(auth);
        let json = serde_json::to_string(&response)?;
        let _ = sender.send(WsMessage::Text(json.into()));
        return Ok(true); // Don't disconnect, just deny this request
    }
    drop(auth);

    // Re-acquire broker lock for publishing
    let mut broker_lock = broker.lock().expect("Broker lock poisoned");

    // Create and publish the message
    let timestamp = chrono::Utc::now().timestamp_millis();
    let msg = popsub_broker::message::Message {
        topic: topic.to_string(),
        payload,
        timestamp,
        message_id: message_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        qos: qos.unwrap_or(0),
    };

    broker_lock.publish(msg);
    info!(client_id = %client_id, username = %username, topic = %topic, "published");

    Ok(true)
}

/// Handle ACK for QoS 1 messages.
fn handle_ack(broker: &Arc<Mutex<Broker>>, client_id: &str, message_id: &str) -> Result<bool> {
    let mut broker_lock = broker.lock().expect("Broker lock poisoned");

    let client = match broker_lock.clients.get(client_id) {
        Some(c) => c,
        None => {
            warn!(client_id = %client_id, "Client not found");
            return Ok(false);
        }
    };

    // Check authentication
    if !client.authenticated {
        warn!(client_id = %client_id, "ack attempt before authentication");
        let response = ServerMessage::Error {
            message: "must authenticate first".to_string(),
        };
        let json = serde_json::to_string(&response)?;
        let _ = client.sender.send(WsMessage::Text(json.into()));
        return Ok(false);
    }

    broker_lock.handle_ack(message_id);
    Ok(true)
}

/// Send a server message to a specific client.
fn send_to_client(
    broker: &Arc<Mutex<Broker>>,
    client_id: &str,
    message: ServerMessage,
) -> Result<()> {
    let json = serde_json::to_string(&message)?;
    let broker_lock = broker.lock().expect("Broker lock poisoned");

    if let Some(client) = broker_lock.clients.get(client_id) {
        client
            .sender
            .send(WsMessage::Text(json.into()))
            .map_err(|e| {
                error!(client_id = %client_id, "Failed to send message: {}", e);
                PopSubError::Client("Failed to send message".to_string())
            })?;
    }

    Ok(())
}

/// Spawn a task that takes messages from the broker->client channel and sends
/// them over the WebSocket.
fn spawn_send_loop<S, F>(
    mut ws_sender: S,
    mut rx: mpsc::UnboundedReceiver<WsMessage>,
    client_id: String,
    do_cleanup: F,
) where
    S: futures_util::Sink<WsMessage, Error = tungstenite::Error> + Unpin + Send + 'static,
    F: Fn() + Send + Clone + 'static,
{
    spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = ws_sender.send(msg).await {
                error!(client_id = %client_id, %e, "Failed to send message");
                break;
            }
        }

        do_cleanup();
        info!(client_id = %client_id, "Send loop closed");
    });
}

// ============================================================================
// Legacy API for backward compatibility
// ============================================================================

use popsub_config::Settings;

/// Start the WebSocket server using Settings (backward-compatible API).
///
/// This creates an AuthService from the settings and delegates to the new API.
/// For new code, prefer using `start_websocket_server_with_auth` directly.
pub async fn start_websocket_server(addr: String, broker: Arc<Mutex<Broker>>, settings: Settings) {
    use popsub_auth::AuthConfig;

    let config = AuthConfig::new(&settings.server.jwt_secret)
        .with_expiration(settings.server.jwt_expiration_hours)
        .with_admin(&settings.server.username, &settings.server.password)
        .with_registration(settings.server.allow_registration)
        .with_default_role(registration_role(&settings.server.registration_role));

    let auth_service = Arc::new(RwLock::new(AuthService::new(config).await));

    start_websocket_server_with_auth(addr, broker, auth_service).await
}

/// Maps the `server.registration_role` setting to the role self-registered
/// users receive.
///
/// Args:
///   name: `none`, `readonly` or `full`. Anything else is treated as `none`
///     and logged, so a typo never grants more access than intended.
///
/// Returns:
///   The default role for new users.
pub fn registration_role(name: &str) -> popsub_auth::DefaultUserRole {
    use popsub_auth::DefaultUserRole;
    match name {
        "full" => DefaultUserRole::FullAccess,
        "readonly" => DefaultUserRole::ReadOnly,
        "none" => DefaultUserRole::LimitedAccess(Vec::new()),
        other => {
            warn!(role = %other, "unknown registration_role, giving new users no topics");
            DefaultUserRole::LimitedAccess(Vec::new())
        }
    }
}

#[cfg(test)]
mod limiter_tests {
    use super::*;

    #[test]
    fn taken_usernames_are_not_revealed() {
        let taken = AuthError::UserAlreadyExists("alice".to_string());
        assert_eq!(registration_error_message(&taken), "registration failed");

        let invalid = AuthError::PermissionDenied("username must be 3 to 32 characters".into());
        assert_eq!(
            registration_error_message(&invalid),
            "username must be 3 to 32 characters"
        );
    }

    #[test]
    fn unknown_registration_roles_grant_nothing() {
        use popsub_auth::DefaultUserRole;
        assert!(matches!(
            registration_role("full"),
            DefaultUserRole::FullAccess
        ));
        assert!(matches!(
            registration_role("readonly"),
            DefaultUserRole::ReadOnly
        ));
        for name in ["none", "admin", "Full", ""] {
            assert!(
                matches!(registration_role(name), DefaultUserRole::LimitedAccess(ref topics) if topics.is_empty()),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn pruning_forgets_idle_message_buckets() {
        let limiters = RateLimiters::default();
        limiters.message_limiter.try_acquire("alice").await;
        assert_eq!(limiters.message_limiter.tracked_keys(), 1);
        assert_eq!(limiters.prune_idle(), 0);
        assert_eq!(limiters.message_limiter.prune_idle(Duration::ZERO), 1);
    }
}
