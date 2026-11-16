//! Turning stored records and protocol frames into WebSocket messages.
//!
//! The frame types themselves live in `popsub_protocol`, shared with the
//! transport and client SDKs. The broker writes deliveries, `subscribed`,
//! `truncated` and `stalled` frames straight to subscribers' channels.

use popsub_protocol::{Delivery, ServerFrame, ServerMessage};
use popsub_storage::Record;
use tracing::error;
use tungstenite::protocol::Message as WsMessage;

/// Builds the frame a subscriber receives for a stored record. The
/// publisher's username is left out.
///
/// Args:
///     record: The stored record.
///
/// Returns:
///     A `message` frame with no request id.
pub(crate) fn delivery(record: &Record) -> ServerFrame {
    ServerFrame::event(ServerMessage::Message(Delivery {
        topic: record.topic.clone(),
        seq: record.seq,
        message_id: record.message_id.clone(),
        payload: record.payload.clone(),
        qos: record.qos,
        published_at_ms: record.published_at_ms,
    }))
}

/// Serialises a frame as a WebSocket text message.
///
/// Args:
///     frame: The frame.
///
/// Returns:
///     The message, or `None` (after logging) if serialisation fails, which
///     only happens if the frame types are changed incorrectly.
pub(crate) fn to_ws(frame: &ServerFrame) -> Option<WsMessage> {
    match serde_json::to_string(frame) {
        Ok(json) => Some(WsMessage::text(json)),
        Err(e) => {
            error!(%e, "failed to serialise a server frame");
            None
        }
    }
}
