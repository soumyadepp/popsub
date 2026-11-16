//! WebSocket server that turns JSON protocol frames into broker operations.
//!
//! Each accepted connection gets a `Client` registered with the `Broker`, a
//! reader loop that parses frames and dispatches them, and a send loop that
//! drains the client's outgoing channel onto the socket. Only `login`,
//! `register` and `auth` are accepted before the connection presents a valid
//! JWT; anything else sent first, or a failed `auth`, closes the connection.
//! Subscribe and publish are then checked against the user's topic permissions
//! in `AuthService`, and login, registration and publishing are rate limited.
//!
//! The broker is a `std::sync::Mutex` shared by every connection, so no handler
//! holds it across an `.await`. Handlers that need both the broker and the auth
//! service read what they need from the broker, release it, await the auth
//! check, and then take the broker lock again.

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

/// The rate limiters one WebSocket server shares across all its connections.
///
/// Each limiter is behind an `Arc`, so cloning this struct shares the same
/// counters rather than copying them.
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

/// How often the server's background task prunes idle rate limiter entries.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);
/// How long a login limiter key may be idle before it is forgotten. It matches
/// the longest lockout, so pruning never ends a lockout early.
const LOGIN_IDLE: Duration = Duration::from_secs(3600);
/// How long a message bucket may be idle before it is forgotten. A bucket
/// refills fully well within this time, so forgetting it changes nothing.
const MESSAGE_IDLE: Duration = Duration::from_secs(600);

impl RateLimiters {
    /// Removes entries for keys that have been quiet long enough to forget.
    ///
    /// Login entries are kept for `LOGIN_IDLE` and message buckets for
    /// `MESSAGE_IDLE`. The registration window drops entries whose attempts
    /// have all aged out of its own window.
    ///
    /// Returns:
    ///     The total number of entries removed across all four limiters.
    pub fn prune_idle(&self) -> usize {
        self.login_limiter.prune_idle(LOGIN_IDLE)
            + self.ip_login_limiter.prune_idle(LOGIN_IDLE)
            + self.register_limiter.prune_idle()
            + self.message_limiter.prune_idle(MESSAGE_IDLE)
    }
}

impl Default for RateLimiters {
    /// Builds the limits the server uses unless it is given others.
    ///
    /// An address and username pair is locked out after 5 failed logins for
    /// 5 minutes, and an address is locked out after 20 failed logins across
    /// any usernames for 15 minutes. Both lockouts back off exponentially up to
    /// 1 hour. An address may make 5 registration attempts per hour, and a user
    /// may publish 100 messages per second with a burst of 50.
    ///
    /// Returns:
    ///     The default set of limiters.
    fn default() -> Self {
        Self {
            login_limiter: Arc::new(
                LoginRateLimiter::builder()
                    .max_attempts(5)
                    .lockout_duration(Duration::from_secs(300))
                    .exponential_backoff(true)
                    .max_lockout_duration(Duration::from_secs(3600))
                    .build(),
            ),
            ip_login_limiter: Arc::new(
                LoginRateLimiter::builder()
                    .max_attempts(20)
                    .lockout_duration(Duration::from_secs(900))
                    .exponential_backoff(true)
                    .max_lockout_duration(Duration::from_secs(3600))
                    .build(),
            ),
            register_limiter: Arc::new(SlidingWindow::new(5, Duration::from_secs(3600))),
            message_limiter: Arc::new(TokenBucket::new(100, 50, Duration::from_secs(1))),
        }
    }
}

/// Runs the WebSocket server with an existing auth service and the default
/// rate limits from `RateLimiters::default`.
///
/// Panics if `addr` can't be bound. See
/// `start_websocket_server_with_rate_limiting` for how connections are served.
///
/// Args:
///     addr: The address to listen on, such as `127.0.0.1:8080`.
///     broker: The broker shared by every connection.
///     auth_service: Handles register, login and token checks, and decides
///         which topics each user may subscribe and publish to.
pub async fn start_websocket_server_with_auth(
    addr: String,
    broker: Arc<Mutex<Broker>>,
    auth_service: Arc<RwLock<AuthService>>,
) {
    start_websocket_server_with_rate_limiting(addr, broker, auth_service, RateLimiters::default())
        .await
}

/// Runs the WebSocket server with an existing auth service and the given rate
/// limiters.
///
/// Spawns a background task that prunes idle limiter entries every
/// `PRUNE_INTERVAL`, then accepts connections until `accept` fails. Each
/// connection runs in its own task: after the WebSocket handshake it registers
/// a `Client` with the broker, starts the send loop, and reads text frames,
/// parsing each one before taking any lock and passing it to `handle_message`.
/// Frames that aren't valid JSON protocol messages are logged and skipped.
/// The connection closes when the socket ends, when a handler asks to
/// disconnect, or when a handler returns an error, in which case the client is
/// first sent an `internal server error` frame. The client is removed from the
/// broker exactly once, by whichever of the reader and send loops finishes
/// first, guarded by an `AtomicBool`.
///
/// Panics if `addr` can't be bound or the broker mutex is poisoned.
///
/// Args:
///     addr: The address to listen on, such as `127.0.0.1:8080`.
///     broker: The broker shared by every connection.
///     auth_service: Handles register, login and token checks, and decides
///         which topics each user may subscribe and publish to.
///     rate_limiters: The limiters shared by every connection on this server.
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

/// Dispatches a parsed client frame to the handler for its type.
///
/// Panics if the broker mutex is poisoned.
///
/// Args:
///     broker: The broker shared by every connection.
///     auth_service: The service used for register, login, token and
///         permission checks.
///     rate_limiters: The limiters shared by every connection.
///     client_id: The broker id of the connection that sent the frame.
///     peer_ip: The connection's remote address, used as the rate limit key
///         for login and registration.
///     msg: The parsed frame.
///
/// Returns:
///     `true` to keep reading from the connection, `false` to close it.
///
/// Raises:
///     PopSubError::Serialization: When a response frame can't be serialised.
///     PopSubError::Client: When a response can't be queued because the
///         client's outgoing channel is closed.
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

/// Creates a user account and replies with a `register_response`.
///
/// Every attempt counts against the registration limit for `peer_ip`, whether
/// or not it succeeds. Over the limit, the reply says how many seconds to wait
/// and the auth service isn't called. A failed registration is reported with
/// `registration_error_message`, which doesn't reveal whether the username is
/// taken. The connection always stays open. The auth service read lock is
/// still held while the reply is queued under the broker lock. Panics if the
/// broker mutex is poisoned.
///
/// Args:
///     broker: The broker, used to find the client to reply to.
///     auth_service: The service that creates the account.
///     rate_limiters: Holds the per address registration limiter.
///     client_id: The broker id of the connection that sent the request.
///     peer_ip: The connection's remote address, the rate limit key.
///     username: The requested username.
///     password: The requested password.
///
/// Returns:
///     Always `true`, to keep the connection open.
///
/// Raises:
///     PopSubError::Serialization: When the reply can't be serialised.
///     PopSubError::Client: When the reply can't be queued because the
///         client's outgoing channel is closed.
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
///     error: The error from the auth service.
///
/// Returns:
///     The reason from a `PermissionDenied` error, or `registration failed`
///     for any other error.
fn registration_error_message(error: &AuthError) -> String {
    match error {
        AuthError::PermissionDenied(reason) => reason.clone(),
        _ => "registration failed".to_string(),
    }
}

/// Checks a username and password and replies with a JWT or an error.
///
/// The request is refused with a `Too many login attempts` error, without
/// calling the auth service, while either `peer_ip` or the `peer_ip|username`
/// pair is locked out. The address is checked first. Only
/// `AuthError::InvalidCredentials` counts as a failure against both limiters,
/// and a success clears the pair's failures (the address count is left as it
/// is). Every login error is reported to the client as `invalid credentials`.
/// The connection always stays open. The auth service read lock is still held
/// while the reply is queued under the broker lock. Panics if the broker
/// mutex is poisoned.
///
/// Args:
///     broker: The broker, used to find the client to reply to.
///     auth_service: The service that checks the credentials and issues the
///         token.
///     rate_limiters: Holds the per pair and per address login limiters.
///     client_id: The broker id of the connection that sent the request.
///     peer_ip: The connection's remote address.
///     username: The username to log in as.
///     password: The password to check.
///
/// Returns:
///     Always `true`, to keep the connection open.
///
/// Raises:
///     PopSubError::Serialization: When the reply can't be serialised.
///     PopSubError::Client: When the reply can't be queued because the
///         client's outgoing channel is closed.
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

/// Validates a JWT and marks the connection as authenticated as its subject.
///
/// On success the auth service read lock is released before the broker lock
/// is taken, then the client's `authenticated` flag and `username` are set and
/// an `authenticated` frame is queued. If the client is no longer registered
/// nothing is sent. An invalid or expired token gets an `authentication
/// failed` error and closes the connection. Panics if the broker mutex is
/// poisoned.
///
/// Args:
///     broker: The broker holding the client record to update.
///     auth_service: The service that validates the token.
///     client_id: The broker id of the connection that sent the token.
///     token: The JWT from a previous `login`.
///
/// Returns:
///     `true` when the token was valid, `false` to close the connection.
///
/// Raises:
///     PopSubError::Serialization: When a reply can't be serialised.
///     PopSubError::Client: When the failure reply can't be queued because
///         the client's outgoing channel is closed.
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
            drop(auth);

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

/// Subscribes the connection to a topic if its user may read it.
///
/// The broker lock is taken to check that the client is authenticated and to
/// copy its username and sender, then released before the auth service is
/// awaited, and taken again to subscribe. An unauthenticated client gets a
/// `must authenticate first` error and the connection is closed. A user without
/// permission for the topic gets a `not authorized` error and stays connected.
/// Subscribing replays the topic's stored messages to the client. Panics if
/// the broker mutex is poisoned.
///
/// Args:
///     broker: The broker to subscribe on.
///     auth_service: The service that checks topic permissions.
///     client_id: The broker id of the connection that sent the request.
///     topic: The topic to subscribe to.
///
/// Returns:
///     `true` to keep the connection open, `false` when the client is not
///     authenticated or no longer registered.
///
/// Raises:
///     PopSubError::Serialization: When an error reply can't be serialised.
async fn handle_subscribe(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    topic: &str,
) -> Result<bool> {
    let (username, sender) = {
        let broker_lock = broker.lock().expect("Broker lock poisoned");

        let client = match broker_lock.clients.get(client_id) {
            Some(c) => c,
            None => {
                warn!(client_id = %client_id, "Client not found");
                return Ok(false);
            }
        };

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
    };

    let auth = auth_service.read().await;
    if !auth.can_subscribe(&username, topic).await {
        warn!(client_id = %client_id, username = %username, topic = %topic, "subscribe denied");
        let response = ServerMessage::Error {
            message: format!("not authorized to subscribe to '{topic}'"),
        };
        drop(auth);
        let json = serde_json::to_string(&response)?;
        let _ = sender.send(WsMessage::Text(json.into()));
        return Ok(true);
    }
    drop(auth);

    let mut broker_lock = broker.lock().expect("Broker lock poisoned");
    broker_lock.subscribe(topic, client_id.to_string());
    info!(client_id = %client_id, username = %username, topic = %topic, "subscribed");

    Ok(true)
}

/// Unsubscribes the connection from a topic.
///
/// No permission check is made, since leaving a topic grants nothing.
/// Unsubscribing from a topic the client isn't on, or one that doesn't exist,
/// does nothing. An unauthenticated client gets a `must authenticate first`
/// error and the connection is closed. Panics if the broker mutex is
/// poisoned.
///
/// Args:
///     broker: The broker to unsubscribe on.
///     client_id: The broker id of the connection that sent the request.
///     topic: The topic to leave.
///
/// Returns:
///     `true` to keep the connection open, `false` when the client is not
///     authenticated or no longer registered.
///
/// Raises:
///     PopSubError::Serialization: When the error reply can't be serialised.
fn handle_unsubscribe(broker: &Arc<Mutex<Broker>>, client_id: &str, topic: &str) -> Result<bool> {
    let mut broker_lock = broker.lock().expect("Broker lock poisoned");

    let client = match broker_lock.clients.get(client_id) {
        Some(c) => c,
        None => {
            warn!(client_id = %client_id, "Client not found");
            return Ok(false);
        }
    };

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

/// Publishes a payload to a topic if the user may write to it and is within
/// the message rate limit.
///
/// Authentication is checked first, under the broker lock, so unauthenticated
/// clients can't spend the rate limit tokens of real users. An unauthenticated
/// client gets a `must authenticate first` error and the connection is closed.
/// The broker lock is then released while the per user rate limit and the
/// topic permission are awaited, in that order, so a denied publish still uses
/// a token. Both a rate limited and a denied publish get an error frame and
/// keep the connection open. Otherwise the broker lock is taken again and the
/// message is published with a random UUID if `message_id` is missing and
/// QoS 0 if `qos` is missing. The broker overwrites the timestamp set here.
/// Panics if the broker mutex is poisoned.
///
/// Args:
///     broker: The broker to publish on.
///     auth_service: The service that checks topic permissions.
///     rate_limiters: Holds the per user message limiter.
///     client_id: The broker id of the connection that sent the request.
///     topic: The topic to publish to.
///     payload: The message body.
///     message_id: The client's id for the message, used to match its `ack`.
///     qos: The delivery level, 0 or 1.
///
/// Returns:
///     `true` to keep the connection open, `false` when the client is not
///     authenticated or no longer registered.
///
/// Raises:
///     PopSubError::Serialization: When an error reply can't be serialised.
///     PopSubError::Client: When the rate limit reply can't be queued because
///         the client's outgoing channel is closed.
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
    let (username, sender) = {
        let broker_lock = broker.lock().expect("Broker lock poisoned");

        let client = match broker_lock.clients.get(client_id) {
            Some(c) => c,
            None => {
                warn!(client_id = %client_id, "Client not found");
                return Ok(false);
            }
        };

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
    };

    if !rate_limiters.message_limiter.try_acquire(&username).await {
        warn!(client_id = %client_id, "message rate limited");
        let response = ServerMessage::Error {
            message: "Rate limited. Too many messages.".to_string(),
        };
        send_to_client(broker, client_id, response)?;
        return Ok(true);
    }

    let auth = auth_service.read().await;
    if !auth.can_publish(&username, topic).await {
        warn!(client_id = %client_id, username = %username, topic = %topic, "publish denied");
        let response = ServerMessage::Error {
            message: format!("not authorized to publish to '{topic}'"),
        };
        drop(auth);
        let json = serde_json::to_string(&response)?;
        let _ = sender.send(WsMessage::Text(json.into()));
        return Ok(true);
    }
    drop(auth);

    let mut broker_lock = broker.lock().expect("Broker lock poisoned");

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

/// Acknowledges a QoS 1 message so the broker stops retrying it.
///
/// The ack is passed to the broker by message id alone, without checking that
/// the message was delivered to this client. An unauthenticated client gets a
/// `must authenticate first` error and the connection is closed. Panics if
/// the broker mutex is poisoned.
///
/// Args:
///     broker: The broker holding the pending acks.
///     client_id: The broker id of the connection that sent the ack.
///     message_id: The id of the message being acknowledged.
///
/// Returns:
///     `true` to keep the connection open, `false` when the client is not
///     authenticated or no longer registered.
///
/// Raises:
///     PopSubError::Serialization: When the error reply can't be serialised.
fn handle_ack(broker: &Arc<Mutex<Broker>>, client_id: &str, message_id: &str) -> Result<bool> {
    let mut broker_lock = broker.lock().expect("Broker lock poisoned");

    let client = match broker_lock.clients.get(client_id) {
        Some(c) => c,
        None => {
            warn!(client_id = %client_id, "Client not found");
            return Ok(false);
        }
    };

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

/// Queues a server frame on a client's outgoing channel.
///
/// The frame is serialised before the broker lock is taken. If the client is
/// no longer registered, nothing is sent and no error is returned. Panics if
/// the broker mutex is poisoned.
///
/// Args:
///     broker: The broker holding the client's sender.
///     client_id: The broker id of the client to send to.
///     message: The frame to send.
///
/// Raises:
///     PopSubError::Serialization: When the frame can't be serialised.
///     PopSubError::Client: When the client's outgoing channel is closed.
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

/// Spawns a task that forwards frames from a client's outgoing channel to its
/// WebSocket.
///
/// The task stops when every sender for the channel has been dropped or when a
/// write to the socket fails, then runs `do_cleanup` to remove the client from
/// the broker.
///
/// Args:
///     ws_sender: The write half of the client's WebSocket.
///     rx: The receiving end of the channel the broker and handlers send to.
///     client_id: The client's broker id, for logging.
///     do_cleanup: Removes the client from the broker. It is shared with the
///         reader loop and does its work only on the first call.
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

use popsub_config::Settings;

/// Runs the WebSocket server configured from the application settings.
///
/// This is the entry point the `popsub` binary uses. It builds an
/// `AuthService` from `settings.server`: the JWT secret and expiry, the admin
/// user from `username` and `password`, whether self-registration is allowed,
/// and the role new users get (see `registration_role`). It then serves with
/// the default rate limits through `start_websocket_server_with_auth`. Panics
/// if `addr` can't be bound.
///
/// Args:
///     addr: The address to listen on, such as `127.0.0.1:8080`.
///     broker: The broker shared by every connection.
///     settings: The loaded configuration. Only the `server` section is read.
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
///     name: `none`, `readonly` or `full`, matched case sensitively. Anything
///         else is treated as `none` and logged, so a typo never grants more
///         access than intended.
///
/// Returns:
///     The default role for new users: full access, read only, or limited
///     access to no topics.
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

/// Unit tests for the private helpers in this file: registration error text,
/// registration role parsing and rate limiter pruning.
#[cfg(test)]
mod limiter_tests {
    use super::*;

    /// Checks that a taken username gives a generic error while validation
    /// reasons are passed through.
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

    /// Checks that only the exact names `full` and `readonly` grant topics and
    /// every other name maps to no topics.
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

    /// Checks that a fresh message bucket survives the default prune but is
    /// dropped once its idle limit is zero.
    #[tokio::test]
    async fn pruning_forgets_idle_message_buckets() {
        let limiters = RateLimiters::default();
        limiters.message_limiter.try_acquire("alice").await;
        assert_eq!(limiters.message_limiter.tracked_keys(), 1);
        assert_eq!(limiters.prune_idle(), 0);
        assert_eq!(limiters.message_limiter.prune_idle(Duration::ZERO), 1);
    }
}
