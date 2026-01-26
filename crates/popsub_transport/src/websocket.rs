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
use popsub_auth::AuthService;
use popsub_utils::error::{PopSubError, Result};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tracing::{error, info, warn};

use crate::message::{ClientMessage, ServerMessage};
use popsub_broker::engine::Broker;
use popsub_client::Client;

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
    let listener = TcpListener::bind(addr.clone()).await.expect("Can't bind");

    info!("WebSocket server listening on ws://{}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let broker = broker.clone();
        let auth_service = auth_service.clone();

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
                    let result = handle_message(&broker, &auth_service, &client_id, parsed);

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
fn handle_message(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    msg: ClientMessage,
) -> Result<bool> {
    match msg {
        ClientMessage::Login { username, password } => {
            handle_login(broker, auth_service, client_id, &username, &password)
        }
        ClientMessage::Register { username, password } => {
            handle_register(broker, auth_service, client_id, &username, &password)
        }
        ClientMessage::Auth { token } => handle_auth(broker, auth_service, client_id, &token),
        ClientMessage::Subscribe { topic } => {
            handle_subscribe(broker, auth_service, client_id, &topic)
        }
        ClientMessage::Unsubscribe { topic } => handle_unsubscribe(broker, client_id, &topic),
        ClientMessage::Publish {
            topic,
            payload,
            message_id,
            qos,
        } => handle_publish(
            broker,
            auth_service,
            client_id,
            &topic,
            payload,
            message_id,
            qos,
        ),
        ClientMessage::Ack { message_id } => handle_ack(broker, client_id, &message_id),
    }
}

/// Handle registration request - create a new user account.
fn handle_register(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    username: &str,
    password: &str,
) -> Result<bool> {
    let mut auth = auth_service.write().map_err(|_| PopSubError::Lock)?;

    let response = match auth.register_user(username, password) {
        Ok(()) => {
            info!(client_id = %client_id, username = %username, "registration successful");
            ServerMessage::RegisterResponse {
                success: true,
                message: "registration successful".to_string(),
            }
        }
        Err(e) => {
            warn!(client_id = %client_id, username = %username, "registration failed: {}", e);
            ServerMessage::RegisterResponse {
                success: false,
                message: e.to_string(),
            }
        }
    };

    drop(auth);
    send_to_client(broker, client_id, response)?;
    Ok(true)
}

/// Handle login request - authenticate user and return JWT token.
fn handle_login(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    username: &str,
    password: &str,
) -> Result<bool> {
    let auth = auth_service.read().map_err(|_| PopSubError::Lock)?;

    let response = match auth.login(username, password) {
        Ok(token) => {
            info!(client_id = %client_id, username = %username, "login successful");
            ServerMessage::LoginResponse { token }
        }
        Err(e) => {
            warn!(client_id = %client_id, username = %username, "login failed: {}", e);
            ServerMessage::Error {
                message: "invalid credentials".to_string(),
            }
        }
    };

    send_to_client(broker, client_id, response)?;
    Ok(true)
}

/// Handle auth request - validate JWT token and mark client as authenticated.
fn handle_auth(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    token: &str,
) -> Result<bool> {
    let auth = auth_service.read().map_err(|_| PopSubError::Lock)?;

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
            drop(auth);
            send_to_client(broker, client_id, response)?;
            Ok(false)
        }
    }
}

/// Handle subscribe request with authorization check.
fn handle_subscribe(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    topic: &str,
) -> Result<bool> {
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
        warn!(client_id = %client_id, "subscribe attempt before authentication");
        let response = ServerMessage::Error {
            message: "must authenticate first".to_string(),
        };
        let json = serde_json::to_string(&response)?;
        let _ = client.sender.send(WsMessage::Text(json.into()));
        return Ok(false);
    }

    let username = client.username.clone().unwrap_or_default();
    let sender = client.sender.clone();

    // Check authorization
    let auth = auth_service.read().map_err(|_| PopSubError::Lock)?;
    if !auth.can_subscribe(&username, topic) {
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

    // Perform subscription
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

/// Handle publish request with authorization check.
fn handle_publish(
    broker: &Arc<Mutex<Broker>>,
    auth_service: &Arc<RwLock<AuthService>>,
    client_id: &str,
    topic: &str,
    payload: String,
    message_id: Option<String>,
    qos: Option<u8>,
) -> Result<bool> {
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
        warn!(client_id = %client_id, "publish attempt before authentication");
        let response = ServerMessage::Error {
            message: "must authenticate first".to_string(),
        };
        let json = serde_json::to_string(&response)?;
        let _ = client.sender.send(WsMessage::Text(json.into()));
        return Ok(false);
    }

    let username = client.username.clone().unwrap_or_default();
    let sender = client.sender.clone();

    // Check authorization
    let auth = auth_service.read().map_err(|_| PopSubError::Lock)?;
    if !auth.can_publish(&username, topic) {
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
        .with_admin(&settings.server.username, &settings.server.password);

    let auth_service = Arc::new(RwLock::new(AuthService::new(config)));

    start_websocket_server_with_auth(addr, broker, auth_service).await
}
