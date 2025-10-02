//! WebSocket transport
//!
//! This file implements a minimal WebSocket server that translates protocol
//! JSON messages into broker operations. Responsibilities:
//! - Accept TCP/WebSocket connections
//! - Create a `Client` for each connection and register it with the `Broker`
//! - Enforce a login -> auth -> other-message order: clients must authenticate
//!   before subscribing or publishing
//! - Serialize/deserialize JSON messages and forward them to the broker
//!
//! Security note: the example uses a simple username/password and JWT signing
//! based on a secret from configuration. For production, prefer secure secret
//! rotation and stronger authentication/authorization controls.

use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{EncodingKey, Header};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracing::{error, info, warn};

use crate::message::{Claims, ClientMessage, ServerMessage};
use jsonwebtoken::{DecodingKey, Validation, decode, encode};
use popsub_broker::engine::Broker;
use popsub_client::Client;
use popsub_config::Settings;

pub async fn start_websocket_server(addr: String, broker: Arc<Mutex<Broker>>, settings: Settings) {
    let listener = TcpListener::bind(addr.clone()).await.expect("Can't bind");

    info!("WebSocket server listening on ws://{}", addr);

    /// Actions that need to be applied to the broker after handling client-local logic.
    enum BrokerAction {
        Subscribe(String),
        Unsubscribe(String),
        Publish(popsub_broker::message::Message),
        Ack(String),
    }

    /// Handle the parts of a parsed client message that only need access to the
    /// per-client state (and can run while holding a mutable borrow to the client).
    /// Returns (continue_flag, optional BrokerAction).
    fn handle_parsed_message(
        client: &mut Client,
        msg: ClientMessage,
        encoding_key: &EncodingKey,
        decoding_key: &DecodingKey,
        validation: &Validation,
        _settings: &Settings,
    ) -> (bool, Option<BrokerAction>) {
        match msg {
            ClientMessage::Login { username, password } => {
                if username == "admin" && password == "password" {
                    let claims = Claims {
                        sub: username.clone(),
                        exp: (chrono::Utc::now() + chrono::Duration::hours(24)).timestamp()
                            as usize,
                    };
                    let token = encode(&Header::default(), &claims, encoding_key).unwrap();

                    let response = ServerMessage::LoginResponse { token };
                    let _ = client.sender.send(WsMessage::Text(
                        serde_json::to_string(&response).unwrap().into(),
                    ));
                } else {
                    let response = ServerMessage::Error {
                        message: "invalid credentials".to_string(),
                    };
                    let _ = client.sender.send(WsMessage::Text(
                        serde_json::to_string(&response).unwrap().into(),
                    ));
                }
                (true, None)
            }
            ClientMessage::Auth { token } => {
                match decode::<Claims>(&token, decoding_key, validation) {
                    Ok(_) => {
                        client.authenticated = true;
                        info!(client_id = %client.id, "authenticated successfully");
                        let response = ServerMessage::Authenticated {};
                        let _ = client.sender.send(WsMessage::Text(
                            serde_json::to_string(&response).unwrap().into(),
                        ));
                        (true, None)
                    }
                    Err(_) => {
                        warn!(client_id = %client.id, "authentication failed");
                        let response = ServerMessage::Error {
                            message: "authentication failed".to_string(),
                        };
                        let _ = client.sender.send(WsMessage::Text(
                            serde_json::to_string(&response).unwrap().into(),
                        ));
                        (false, None)
                    }
                }
            }
            _ if !client.authenticated => {
                warn!(client_id = %client.id, "sent message before authentication");
                let response = ServerMessage::Error {
                    message: "must authenticate first".to_string(),
                };
                let _ = client.sender.send(WsMessage::Text(
                    serde_json::to_string(&response).unwrap().into(),
                ));
                (false, None)
            }
            ClientMessage::Subscribe { topic } => (true, Some(BrokerAction::Subscribe(topic))),
            ClientMessage::Unsubscribe { topic } => (true, Some(BrokerAction::Unsubscribe(topic))),
            ClientMessage::Publish {
                topic,
                payload,
                message_id,
                qos,
            } => {
                let timestamp = chrono::Utc::now().timestamp_millis();
                let msg = popsub_broker::message::Message {
                    topic: topic.clone(),
                    payload,
                    timestamp,
                    message_id: message_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    qos: qos.unwrap_or(0),
                };
                (true, Some(BrokerAction::Publish(msg)))
            }
            ClientMessage::Ack { message_id } => (true, Some(BrokerAction::Ack(message_id))),
        }
    }

    /// Spawn a task that takes messages from the broker->client channel and sends
    /// them over the WebSocket. Kept generic over the sink type to avoid pulling
    /// concrete stream types into the signature.
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

    while let Ok((stream, _)) = listener.accept().await {
        let broker = broker.clone();
        let settings = settings.clone();

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
                let mut broker = broker.lock().unwrap();
                broker.register_client(client);
            }

            let cleanup_called = Arc::new(AtomicBool::new(false));

            let do_cleanup = {
                let broker = broker.clone();
                let client_id = client_id.clone();
                let cleanup_called = cleanup_called.clone();

                move || {
                    if !cleanup_called.swap(true, Ordering::SeqCst) {
                        let mut broker = broker.lock().unwrap();
                        broker.cleanup_client(&client_id);
                    }
                }
            };

            // Precompute JWT keys/validation for this connection so we don't recreate them per-message
            let encoding_key = EncodingKey::from_secret(settings.server.jwt_secret.as_ref());
            let decoding_key = DecodingKey::from_secret(settings.server.jwt_secret.as_ref());
            let validation = Validation::default();

            spawn_send_loop(ws_sender, rx, client_id.clone(), do_cleanup.clone());
            while let Some(Ok(msg)) = ws_receiver.next().await {
                if msg.is_text() {
                    let text = msg.to_text().unwrap();

                    // Parse before taking the broker lock to keep lock scope small
                    let parsed = match serde_json::from_str::<ClientMessage>(text) {
                        Ok(pm) => pm,
                        Err(err) => {
                            error!(client_id = %client_id, "Invalid client message: {} | {}", err, &text.chars().take(100).collect::<String>());
                            continue;
                        }
                    };

                    // Borrow broker to find the client and handle client-local logic
                    let continue_flag: bool;
                    let maybe_action: Option<BrokerAction>;

                    {
                        let mut broker_lock = broker.lock().unwrap();
                        let client = match broker_lock.clients.get_mut(&client_id) {
                            Some(c) => c,
                            None => {
                                warn!(client_id = %client_id, "Client not found in broker during message handling");
                                do_cleanup();
                                break;
                            }
                        };

                        let (cont, action) = handle_parsed_message(
                            client,
                            parsed,
                            &encoding_key,
                            &decoding_key,
                            &validation,
                            &settings,
                        );
                        continue_flag = cont;
                        maybe_action = action;
                    }

                    if !continue_flag {
                        break;
                    }

                    if let Some(action) = maybe_action {
                        let mut broker_lock = broker.lock().unwrap();
                        match action {
                            BrokerAction::Subscribe(topic) => {
                                broker_lock.subscribe(&topic, client_id.clone());
                                info!(client_id = %client_id, topic = %topic, "subscribed");
                            }
                            BrokerAction::Unsubscribe(topic) => {
                                broker_lock.unsubscribe(&topic, &client_id);
                                info!(client_id = %client_id, topic = %topic, "unsubscribed");
                            }
                            BrokerAction::Publish(msg) => {
                                broker_lock.publish(msg);
                                // topic logged by publisher side earlier when message created
                            }
                            BrokerAction::Ack(message_id) => {
                                broker_lock.handle_ack(&message_id);
                            }
                        }
                    }
                }
            }

            do_cleanup();
        });
    }
}
