//! Rust client for PopSub.
//!
//! `Client` keeps a connection to a PopSub server and handles what every
//! application would otherwise have to: logging in and refreshing the token,
//! reconnecting with backoff (and knowing when not to, from the server's
//! close code), resubscribing without gaps, resending confirmed publishes
//! without duplicating them, acknowledging QoS 1 messages, and making sure
//! each `(topic, seq)` reaches the application once.
//!
//! Example:
//!
//! ```no_run
//! use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};
//!
//! # async fn run() -> Result<(), popsub_sdk::Error> {
//! let config = ClientConfig::new("ws://127.0.0.1:8080", Credentials::password("alice", "secret"))
//!     .session("alice-laptop");
//! let client = Client::connect(config).await?;
//!
//! let mut orders = client.subscribe("orders", StartFrom::Resume).await?;
//! let stored = client.publish_confirmed("orders", "order 42 shipped").await?;
//! println!("stored as seq {}", stored.seq);
//!
//! while let Some(message) = orders.next().await {
//!     println!("{} #{}: {}", message.topic, message.seq, message.payload);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The protocol itself is in `popsub_protocol` and documented in the
//! repository's `docs/DESIGN.md` section 4.

mod admin;
mod client;
mod config;
mod driver;
mod error;
mod message;

#[cfg(test)]
mod tests;

pub use admin::Admin;
pub use client::Client;
pub use config::{AckMode, ClientConfig, Credentials, ReconnectPolicy};
pub use driver::Published;
pub use error::Error;
pub use message::{Event, Message, Subscription};
pub use popsub_protocol::{
    Access, CloseCode, Encoding, ErrorCode, RoleKind, RoleSpec, StartFrom, TopicGrant, TopicInfo,
    UserInfo,
};
