use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

use std::sync::{Arc, Mutex};

use crate::broker::{Broker, message::Message};
use crate::transport::message::ClientMessage;

pub async fn start_websocket_server(addr: &str, broker: Arc<Mutex<Broker>>) {
    let listener = TcpListener::bind(addr).await.expect("Can't bind");

    println!("WebSocket server listening on ws://{}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let broker = broker.clone();

        // You can later replace this with a UUID or real session ID
        let client_id = format!("client-{}", uuid::Uuid::new_v4());

        tokio::spawn(async move {
            let ws_stream = match accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("WebSocket handshake error: {}", e);
                    return;
                }
            };

            let (_ws_sender, mut ws_receiver) = ws_stream.split();

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
                                topic,
                                payload,
                                timestamp,
                            });
                        }

                        Err(err) => {
                            eprintln!("Invalid client message: {} | {}", err, text);
                        }
                    }
                }
            }
            // Optional: Cleanup logic here for disconnection
            println!("{} disconnected", client_id);
        });
    }
}
