use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

use std::sync::{Arc, Mutex};

use crate::broker::{Broker, message::Message};
use crate::client::Client;
use crate::transport::message::ClientMessage;

/// Starts the WebSocket server
/// Listens for incoming WebSocket connections
/// Accepts connections and spawns a task for each client
/// Handles client messages, including subscriptions, unsubscriptions, and publishing messages
/// Manages the broker state and ensures messages are sent to the appropriate clients
/// This function is the entry point for the WebSocket server, allowing clients to connect and interact
/// with the broker system through WebSocket messages.
pub async fn start_websocket_server(addr: &str, broker: Arc<Mutex<Broker>>) {
    let listener = TcpListener::bind(addr).await.expect("Can't bind");

    println!("WebSocket server listening on ws://{}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let broker = broker.clone();
        let client_id = format!("client-{}", uuid::Uuid::new_v4());

        tokio::spawn(async move {
            let ws_stream = match accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("WebSocket handshake error: {}", e);
                    return;
                }
            };

            let (mut ws_sender, mut ws_receiver) = ws_stream.split();

            let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();

            // Register the client
            {
                let mut broker = broker.lock().unwrap();
                broker.register_client(Client {
                    id: client_id.clone(),
                    sender: tx.clone(),
                });
            }

            // Cleanup task for receive loop
            let cleanup_client = {
                let broker = broker.clone();
                let client_id = client_id.clone();
                async move {
                    let mut broker = broker.lock().unwrap();
                    broker.cleanup_client(&client_id);
                }
            };

            // Send loop (broker → client) with cleanup on failure
            let broker_clone = broker.clone();
            let client_id_clone = client_id.clone();

            spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if let Err(e) = ws_sender.send(msg).await {
                        eprintln!("Failed to send message to {}: {}", client_id_clone, e);
                        break;
                    }
                }

                let mut broker = broker_clone.lock().unwrap();
                broker.cleanup_client(&client_id_clone);
                println!("Send loop closed for {}", client_id_clone);
            });

            // Handle messages from client
            while let Some(Ok(msg)) = ws_receiver.next().await {
                if msg.is_text() {
                    let text = msg.to_text().unwrap();
                    match serde_json::from_str::<ClientMessage>(text) {
                        Ok(ClientMessage::Subscribe { topic }) => {
                            let mut broker = broker.lock().unwrap();
                            broker.subscribe(&topic, client_id.clone());
                            println!("{} subscribed to {}", client_id, topic);
                        }

                        Ok(ClientMessage::Unsubscribe { topic }) => {
                            let mut broker = broker.lock().unwrap();
                            broker.unsubscribe(&topic, &client_id);
                            println!("{} unsubscribed from {}", client_id, topic);
                        }

                        Ok(ClientMessage::Publish {
                            topic,
                            payload,
                            timestamp,
                        }) => {
                            let broker = broker.lock().unwrap();
                            let printable_topic = topic.clone();
                            broker.publish(Message {
                                topic,
                                payload,
                                timestamp,
                            });
                            println!("{} published to {}", client_id, printable_topic);
                        }

                        Err(err) => {
                            eprintln!(
                                "Invalid client message from {}: {} | {}",
                                client_id,
                                err,
                                &text.chars().take(100).collect::<String>()
                            );
                        }
                    }
                }
            }

            // Cleanup after receive loop exits
            cleanup_client.await;
        });
    }
}
