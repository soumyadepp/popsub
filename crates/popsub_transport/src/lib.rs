//! WebSocket transport for PopSub: the JSON protocol and the server that speaks it.
//!
//! The frames themselves are defined in `popsub_protocol` and re-exported
//! here. `websocket` accepts connections, enforces authentication, topic
//! permissions and rate limits, and turns requests into broker calls. The
//! auth types are re-exported here so the binary can configure users and
//! permissions through this crate without depending on `popsub_auth` directly.

pub mod websocket;

#[cfg(test)]
mod websocket_tests;

pub use popsub_protocol::{ClientFrame, ClientMessage, ServerFrame, ServerMessage};
pub use websocket::{RateLimiters, Server, registration_role};

pub use popsub_auth::service::UserBuilder;
pub use popsub_auth::{AuthConfig, AuthService, Role, TopicPermission};
