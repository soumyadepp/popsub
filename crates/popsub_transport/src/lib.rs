//! WebSocket transport for PopSub: the JSON protocol and the server that speaks it.
//!
//! `message` defines the frames clients and the server exchange, and `websocket`
//! accepts connections, enforces authentication, topic permissions and rate
//! limits, and turns requests into broker calls. The auth types are re-exported
//! here so the binary can configure users and permissions through this crate
//! without depending on `popsub_auth` directly.

pub mod message;
pub mod websocket;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod websocket_tests;

pub use message::{Claims, ClientMessage, ServerMessage};
pub use websocket::{start_websocket_server, start_websocket_server_with_auth};

pub use popsub_auth::service::UserBuilder;
pub use popsub_auth::{AuthConfig, AuthService, Role, TopicPermission};
