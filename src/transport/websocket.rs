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

            // Shared flag to ensure cleanup is only called once
            let cleanup_called = Arc::new(AtomicBool::new(false));

            // Define cleanup logic
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

            // Send loop: broker → client
            {
                let client_id = client_id.clone();
                let do_cleanup = do_cleanup.clone();

                spawn(async move {
                    while let Some(msg) = rx.recv().await {
                        if let Err(e) = ws_sender.send(msg).await {
                            eprintln!("Failed to send message to {}: {}", client_id, e);
                            break;
                        }
                    }

                    do_cleanup();
                    println!("Send loop closed for {}", client_id);
                });
            }

            // Receive loop: client → broker
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
                            broker.publish(Message {
                                topic: topic.clone(),
                                payload,
                                timestamp,
                            });
                            println!("{} published to {}", client_id, topic);
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

            // Run cleanup when receive loop exits
            do_cleanup();
        });
    }
}
