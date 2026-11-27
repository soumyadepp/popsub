//! Turning stored records and protocol frames into WebSocket messages.
//!
//! The frame types themselves live in `popsub_protocol`, shared with the
//! transport and client SDKs. The broker writes deliveries, `subscribed`,
//! `truncated` and `stalled` frames straight to subscribers' channels.

use popsub_protocol::{CloseCode, Delivery, Encoding, ServerFrame, ServerMessage};
use popsub_storage::{PayloadEncoding, Record, push_json_str};
use tracing::error;
use tungstenite::protocol::CloseFrame;
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
        encoding: record.encoding.map(|encoding| match encoding {
            PayloadEncoding::Base64 => Encoding::Base64,
        }),
        qos: record.qos,
        published_at_ms: record.published_at_ms,
    }))
}

/// Serialises the frame a subscriber receives for a stored record, as a
/// WebSocket text message.
///
/// Gives the same text as `to_ws(&delivery(record))`. It writes the frame
/// directly into space reserved up front and copies a payload that needs no
/// escaping straight through, which matters when one record goes to many
/// subscribers a second.
///
/// Args:
///     record: The stored record.
///
/// Returns:
///     The message.
pub(crate) fn delivery_ws(record: &Record) -> Option<WsMessage> {
    let mut integers = itoa::Buffer::new();
    let mut out = String::with_capacity(
        record.topic.len() + record.message_id.len() + record.payload.len() + 144,
    );
    out.push_str("{\"type\":\"message\",\"topic\":");
    push_json_str(&mut out, &record.topic);
    out.push_str(",\"seq\":");
    out.push_str(integers.format(record.seq));
    out.push_str(",\"message_id\":");
    push_json_str(&mut out, &record.message_id);
    out.push_str(",\"payload\":");
    push_json_str(&mut out, &record.payload);
    match record.encoding {
        Some(PayloadEncoding::Base64) => out.push_str(",\"encoding\":\"base64\""),
        None => {}
    }
    out.push_str(",\"qos\":");
    out.push_str(integers.format(record.qos));
    out.push_str(",\"published_at_ms\":");
    out.push_str(integers.format(record.published_at_ms));
    out.push('}');
    Some(WsMessage::text(out))
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

/// Builds the WebSocket close message for a close code.
///
/// Args:
///     code: Why the connection is being closed.
///
/// Returns:
///     A close message carrying the numeric code and its short reason.
pub fn close_message(code: CloseCode) -> WsMessage {
    WsMessage::Close(Some(CloseFrame {
        code: code.code().into(),
        reason: code.reason().into(),
    }))
}
