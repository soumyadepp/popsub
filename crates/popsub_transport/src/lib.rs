//! WebSocket transport for PopSub: the JSON protocol and the server that speaks it.
//!
//! The frames themselves are defined in `popsub_protocol` and re-exported
//! here. `websocket` accepts connections, enforces authentication, topic
//! permissions, rate limits and the connection limit, and turns requests
//! into broker calls. `proxy` works out a client's address behind trusted
//! reverse proxies, for the rate limits, and `tls` serves `wss://`.
//! `health` serves `/healthz` and `/readyz` over plain HTTP. The
//! auth types are re-exported here so the binary can configure users and
//! permissions through this crate without depending on `popsub_auth` directly.

pub mod health;
pub mod proxy;
pub mod tls;
pub mod websocket;

#[cfg(test)]
mod websocket_tests;

pub use health::Health;
pub use popsub_protocol::{ClientFrame, ClientMessage, ServerFrame, ServerMessage};
pub use proxy::TrustedProxies;
pub use tls::ServerTls;
pub use websocket::{RateLimiters, Server, registration_role};

pub use popsub_auth::service::UserBuilder;
pub use popsub_auth::{
    AuthConfig, AuthService, FileUserStore, InMemoryUserStore, Role, TopicPermission, UserStore,
};
