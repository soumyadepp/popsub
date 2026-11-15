pub mod message;
pub mod websocket;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod websocket_tests;

pub use message::{Claims, ClientMessage, ServerMessage};
pub use websocket::{start_websocket_server, start_websocket_server_with_auth};

// Re-export auth types for convenience
pub use popsub_auth::service::UserBuilder;
pub use popsub_auth::{AuthConfig, AuthService, Role, TopicPermission};
