//! How a record is laid out inside a segment frame.
//!
//! Records are written in a compact binary form:
//!
//! | Bytes | Field |
//! |---|---|
//! | 1 | `FORMAT`, the byte that tells this form from JSON |
//! | 1 | Flags: `BASE64` when the payload is base64, `UUID_ID` when the id is stored as 16 bytes |
//! | 1 | QoS |
//! | 8 | `seq`, little endian |
//! | 8 | `published_at_ms`, little endian |
//! | 16, or 2 + n | The message id: a UUID's bytes, or a length and the text |
//! | 2 + n | The publisher: a length and the text |
//! | the rest | The payload |
//!
//! The topic isn't stored: the segment's directory names it. A record
//! with a 128-byte payload and a server-assigned id takes about 180 bytes
//! with its frame, against about 300 as JSON, which matters because the
//! disk is what limits a busy server.
//!
//! Logs written before this form existed hold JSON records, which start
//! with `{`. Both are read; only the binary form is written, except for a
//! record whose id or publisher is too long for a two-byte length, which is
//! written as JSON.

use crate::record::{PayloadEncoding, Record};

/// The first byte of a binary record. A JSON record starts with `{`.
const FORMAT: u8 = 1;

/// The flag for a base64 payload.
const BASE64: u8 = 1;

/// The flag for a message id stored as a UUID's 16 bytes.
const UUID_ID: u8 = 2;

/// The bytes before the variable-length fields.
const FIXED: usize = 19;

/// Appends a record's stored form to a buffer.
///
/// Args:
///     record: The record.
///     out: The buffer to append to.
pub(crate) fn encode(record: &Record, out: &mut Vec<u8>) {
    let id_uuid = uuid_bytes(&record.message_id);
    if (id_uuid.is_none() && record.message_id.len() > usize::from(u16::MAX))
        || record.publisher.len() > usize::from(u16::MAX)
    {
        out.extend_from_slice(record.to_json().as_bytes());
        return;
    }
    let mut flags = 0;
    if record.encoding == Some(PayloadEncoding::Base64) {
        flags |= BASE64;
    }
    if id_uuid.is_some() {
        flags |= UUID_ID;
    }
    out.reserve(FIXED + 20 + record.publisher.len() + record.payload.len());
    out.push(FORMAT);
    out.push(flags);
    out.push(record.qos);
    out.extend_from_slice(&record.seq.to_le_bytes());
    out.extend_from_slice(&record.published_at_ms.to_le_bytes());
    match id_uuid {
        Some(bytes) => out.extend_from_slice(&bytes),
        None => push_text(out, &record.message_id),
    }
    push_text(out, &record.publisher);
    out.extend_from_slice(record.payload.as_bytes());
}

/// Reads a stored record in either form.
///
/// Args:
///     body: The frame's body.
///     topic: The topic whose log holds it, which a binary record doesn't store.
///
/// Returns:
///     The record.
///
/// Raises:
///     String: What is wrong with a body that doesn't decode.
pub(crate) fn decode(body: &[u8], topic: &str) -> Result<Record, String> {
    match body.first() {
        Some(&FORMAT) => decode_binary(body, topic),
        Some(b'{') => serde_json::from_slice(body).map_err(|e| e.to_string()),
        _ => Err("unknown record format".to_string()),
    }
}

/// Reads a record in the binary form.
///
/// Args:
///     body: The frame's body, starting with `FORMAT`.
///     topic: The topic whose log holds it.
///
/// Returns:
///     The record.
///
/// Raises:
///     String: When the body is cut short or a text field isn't UTF-8.
fn decode_binary(body: &[u8], topic: &str) -> Result<Record, String> {
    let fixed = body.get(..FIXED).ok_or("record cut short")?;
    let flags = fixed[1];
    let qos = fixed[2];
    let seq = u64::from_le_bytes(fixed[3..11].try_into().map_err(|_| "bad seq")?);
    let published_at_ms = i64::from_le_bytes(fixed[11..19].try_into().map_err(|_| "bad time")?);
    let mut at = FIXED;
    let message_id = if flags & UUID_ID != 0 {
        let bytes: [u8; 16] = body
            .get(at..at + 16)
            .ok_or("record cut short")?
            .try_into()
            .map_err(|_| "bad id")?;
        at += 16;
        uuid::Uuid::from_bytes(bytes)
            .hyphenated()
            .encode_lower(&mut uuid::Uuid::encode_buffer())
            .to_owned()
    } else {
        read_text(body, &mut at)?
    };
    let publisher = read_text(body, &mut at)?;
    let payload = std::str::from_utf8(&body[at..])
        .map_err(|_| "payload isn't UTF-8")?
        .to_owned();
    Ok(Record {
        topic: topic.to_owned(),
        seq,
        message_id,
        publisher,
        payload,
        encoding: (flags & BASE64 != 0).then_some(PayloadEncoding::Base64),
        qos,
        published_at_ms,
    })
}

/// Returns a message id's 16 bytes when it is a UUID written exactly as the
/// server writes one, lowercase and hyphenated, so it reads back the same.
///
/// Args:
///     id: The message id.
///
/// Returns:
///     The bytes, or `None` for any other id.
fn uuid_bytes(id: &str) -> Option<[u8; 16]> {
    if id.len() != 36 {
        return None;
    }
    let uuid = uuid::Uuid::try_parse(id).ok()?;
    let mut buffer = uuid::Uuid::encode_buffer();
    (uuid.hyphenated().encode_lower(&mut buffer) == id).then(|| *uuid.as_bytes())
}

/// Appends a two-byte length and the text.
///
/// Args:
///     out: The buffer.
///     text: The text, at most `u16::MAX` bytes.
fn push_text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u16).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// Reads a two-byte length and that much text.
///
/// Args:
///     body: The record.
///     at: Where the length starts; moved past the text.
///
/// Returns:
///     The text.
///
/// Raises:
///     String: When the body is cut short or the text isn't UTF-8.
fn read_text(body: &[u8], at: &mut usize) -> Result<String, String> {
    let len = body.get(*at..*at + 2).ok_or("record cut short")?;
    let len = usize::from(u16::from_le_bytes([len[0], len[1]]));
    let start = *at + 2;
    let text = body.get(start..start + len).ok_or("record cut short")?;
    *at = start + len;
    Ok(std::str::from_utf8(text)
        .map_err(|_| "text isn't UTF-8")?
        .to_owned())
}
