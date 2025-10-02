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

use crate::message::{Claims, ClientMessage, ServerMessage};
use jsonwebtoken::{DecodingKey, Validation, decode, encode};
use popsub_broker::engine::Broker;
use popsub_client::Client;
use popsub_config::Settings;

pub async fn start_websocket_server(addr: String, broker: Arc<Mutex<Broker>>, settings: Settings) {
    let listener = TcpListener::bind(addr.clone()).await.expect("Can't bind");

    println!("WebSocket server listening on ws://{addr}");

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
            let (mut ws_sender, mut ws_receiver) = ws_stream.split();
            let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
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

            {
                let client_id = client_id.clone();
                let do_cleanup = do_cleanup.clone();

                spawn(async move {
                    while let Some(msg) = rx.recv().await {
                        if let Err(e) = ws_sender.send(msg).await {
                            eprintln!("Failed to send message to {client_id}: {e}");
                            break;
                        }
                    }

                    do_cleanup();
                    println!("Send loop closed for {client_id}");
                });
            }

            while let Some(Ok(msg)) = ws_receiver.next().await {
                if msg.is_text() {
                    let text = msg.to_text().unwrap();
                    let mut broker_lock = broker.lock().unwrap();
                    let client = broker_lock.clients.get_mut(&client_id).unwrap();

                    match serde_json::from_str::<ClientMessage>(text) {
                        Ok(ClientMessage::Login { username, password }) => {
                            if username == "admin" && password == "password" {
                                let claims = Claims {
                                    sub: username.clone(),
                                    exp: (chrono::Utc::now() + chrono::Duration::hours(24))
                                        .timestamp()
                                        as usize,
                                };
                                let token = encode(
                                    &Header::default(),
                                    &claims,
                                    &EncodingKey::from_secret(settings.server.jwt_secret.as_ref()),
                                )
                                .unwrap();

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
                        }
                        Ok(ClientMessage::Auth { token }) => {
                            let validation = Validation::default();
                            match decode::<Claims>(
                                &token,
                                &DecodingKey::from_secret(settings.server.jwt_secret.as_ref()),
                                &validation,
                            ) {
                                Ok(_) => {
                                    client.authenticated = true;
                                    println!("{client_id} authenticated successfully");
                                    let response = ServerMessage::Authenticated {};
                                    let _ = client.sender.send(WsMessage::Text(
                                        serde_json::to_string(&response).unwrap().into(),
                                    ));
                                }
                                Err(_) => {
                                    eprintln!("{client_id} authentication failed");
                                    let response = ServerMessage::Error {
                                        message: "authentication failed".to_string(),
                                    };
                                    let _ = client.sender.send(WsMessage::Text(
                                        serde_json::to_string(&response).unwrap().into(),
                                    ));
                                    break;
                                }
                            }
                        }
                        Ok(_) if !client.authenticated => {
                            eprintln!("Client {client_id} sent message before authentication");
                            let response = ServerMessage::Error {
                                message: "must authenticate first".to_string(),
                            };
                            let _ = client.sender.send(WsMessage::Text(
                                serde_json::to_string(&response).unwrap().into(),
                            ));
                            break;
                        }
                        Ok(ClientMessage::Subscribe { topic }) => {
                            broker_lock.subscribe(&topic, client_id.clone());
                            println!("{client_id} subscribed to {topic}");
                        }
                        Ok(ClientMessage::Unsubscribe { topic }) => {
                            broker_lock.unsubscribe(&topic, &client_id);
                            println!("{client_id} unsubscribed from {topic}");
                        }
                        Ok(ClientMessage::Publish {
                            topic,
                            payload,
                            message_id,
                            qos,
                        }) => {
                            let timestamp = chrono::Utc::now().timestamp_millis();
                            broker_lock.publish(popsub_broker::message::Message {
                                topic: topic.clone(),
                                payload,
                                timestamp,
                                message_id: message_id
                                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                                qos: qos.unwrap_or(0),
                            });
                            println!("{client_id} published to {topic}");
                        }
                        Ok(ClientMessage::Ack { message_id }) => {
                            broker_lock.handle_ack(&message_id);
                        }
                        Err(err) => {
                            eprintln!(
                                "Invalid client message from {client_id}: {err} | {}",
                                &text.chars().take(100).collect::<String>()
                            );
                        }
                    }
                }
            }

            do_cleanup();
        });
    }
}
