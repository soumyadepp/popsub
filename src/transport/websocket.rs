use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::broker::{Broker, message::Message};
use crate::client::Client;
use crate::transport::message::ClientMessage;

/// Starts the asynchronous WebSocket server and handles client communication.
///
/// Binds to the given address and spawns a new task for each incoming client connection.
/// Each client is identified with a UUID, registered with the broker, and can:
/// - Subscribe to topics
/// - Unsubscribe from topics
/// - Publish messages to topics
///
/// Incoming client messages must conform to the `ClientMessage` enum.
/// Outgoing messages are sent using a Tokio unbounded channel (`mpsc::UnboundedSender`).
///
/// This function also ensures proper cleanup of client subscriptions on disconnects.
///
/// # Arguments
///
/// * `addr` - The address (e.g., `"127.0.0.1:8080"`) to bind the WebSocket server to.
/// * `broker` - A shared, thread-safe instance of the message broker, used to manage topics, clients, and persistence.
///
/// # Panics
///
/// Panics if the TCP listener cannot bind to the provided address.
///
/// # Example
///
/// ```rust
/// let broker = Arc::new(Mutex::new(Broker::new()));
/// start_websocket_server("127.0.0.1:8080", broker).await;
/// ```
///
/// # Notes
/// - This function runs indefinitely until externally terminated.
/// - Each client is cleaned up exactly once (guaranteed by an `AtomicBool`).
pub async fn start_websocket_server(addr: &str, broker: Arc<Mutex<Broker>>) {
    let listener = TcpListener::bind(addr).await.expect("Can't bind");

    println!("WebSocket server listening on ws://{addr}");

    while let Ok((stream, _)) = listener.accept().await {
        let broker = broker.clone();
        let client_id = format!("client-{}", uuid::Uuid::new_v4());

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

            {
                let mut broker = broker.lock().unwrap();
                broker.register_client(Client {
                    id: client_id.clone(),
                    sender: tx.clone(),
                });
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

                    match serde_json::from_str::<ClientMessage>(text) {
                        Ok(ClientMessage::Subscribe { topic }) => {
                            let mut broker = broker.lock().unwrap();
                            broker.subscribe(&topic, client_id.clone());
                            println!("{client_id} subscribed to {topic}");
                        }

                        Ok(ClientMessage::Unsubscribe { topic }) => {
                            let mut broker = broker.lock().unwrap();
                            broker.unsubscribe(&topic, &client_id);
                            println!("{client_id} unsubscribed from {topic}");
                        }

                        Ok(ClientMessage::Publish { topic, payload }) => {
                            let broker = broker.lock().unwrap();
                            let timestamp = chrono::Utc::now().timestamp_millis();
                            broker.publish(Message {
                                topic: topic.clone(),
                                payload,
                                timestamp,
                            });
                            println!("{client_id} published to {topic}");
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

            do_cleanup();
        });
    }
}
