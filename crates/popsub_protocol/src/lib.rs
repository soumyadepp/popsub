//! The PopSub WebSocket protocol: every frame clients and the server exchange.
//!
//! Frames are JSON objects tagged by `type`. When the server closes a
//! connection, the WebSocket close code (`CloseCode`) says why and whether
//! to reconnect. A client frame may carry an
//! `id` of the client's choosing; the reply to it carries the same `id`, so
//! a client can match replies to requests. Frames the server sends on its
//! own (deliveries, `truncated`, `stalled`) have no `id`.
//!
//! The server and client SDKs share these types, so they can't drift apart.
//! The crate depends only on `serde`, so any client can use it. With the
//! `schema` feature it also produces a JSON Schema of every frame (see
//! `schema`), for SDKs in other languages.
//!
//! Evolution rules for `PROTOCOL_VERSION`:
//!
//! - Within a version, frames and fields are only ever added.
//! - Clients ignore fields and frame types they don't know.
//! - Removing or changing the meaning of anything needs a new version.

mod client;
mod close;
#[cfg(feature = "schema")]
pub mod schema;
mod server;

#[cfg(test)]
mod tests;

pub use client::{ClientFrame, ClientMessage, StartFrom};
pub use close::CloseCode;
pub use server::{
    Delivery, ErrorCode, Limits, ServerFrame, ServerMessage, Stalled, Subscribed, Truncated,
};

/// The protocol version this crate describes. A client names the version it
/// speaks in `auth`; the server confirms it in `authenticated`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The id a client attaches to a request, echoed in the reply.
///
/// Either a number or a string, and given back in the same form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum RequestId {
    /// A numeric id, such as a counter.
    Number(u64),
    /// A string id, such as a UUID.
    Text(String),
}
