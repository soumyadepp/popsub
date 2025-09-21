//! The `transport` module is responsible for handling network communication
//! with clients, primarily via WebSockets.
//!
//! It defines the messaging protocol used between clients and the server,
//! and implements the WebSocket server itself, managing connections,
//! message parsing, and forwarding client requests to the broker.

pub mod message;
pub mod websocket;

#[cfg(test)]
mod tests;
