//! The on-disk frame shared by segment files and the session log.
//!
//! A frame is `length (u32, little endian) | crc32 (u32, little endian) |
//! body`, where the CRC covers the body. A frame that is cut short, longer
//! than `MAX_FRAME`, or whose CRC doesn't match is reported as `Torn`; the
//! caller decides whether that is an interrupted write at the end of a
//! file (truncate and carry on) or corruption in the middle (refuse to open).

/// Bytes before the body: the length and the CRC.
pub(crate) const HEADER: usize = 8;

/// The largest body accepted when reading. Anything larger is treated as
/// damage rather than allocated.
pub(crate) const MAX_FRAME: usize = 64 * 1024 * 1024;

/// What was found at an offset in a buffer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decoded<'a> {
    /// A whole, checked frame.
    Frame {
        /// The frame's body.
        body: &'a [u8],
        /// The offset just past the frame.
        next: usize,
    },
    /// The offset is exactly the end of the buffer.
    End,
    /// The bytes at the offset aren't a whole, valid frame.
    Torn,
}

/// Frames a body for writing.
///
/// Args:
///     body: The encoded record or operation.
///
/// Returns:
///     The header followed by the body.
pub(crate) fn encode(body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + body.len());
    encode_into(&mut frame, body);
    frame
}

/// Appends one frame holding `body` to a buffer of frames.
///
/// Args:
///     out: The buffer to append to.
///     body: The bytes to store.
///
/// Returns:
///     The frame's length: the header and the body.
pub(crate) fn encode_into(out: &mut Vec<u8>, body: &[u8]) -> usize {
    out.reserve(HEADER + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(body).to_le_bytes());
    out.extend_from_slice(body);
    HEADER + body.len()
}

/// Reads the frame starting at an offset.
///
/// Args:
///     buf: The file's contents, or a chunk of them.
///     at: Where the frame should start.
///
/// Returns:
///     The frame, the end of the buffer, or `Torn`.
pub(crate) fn decode(buf: &[u8], at: usize) -> Decoded<'_> {
    if at == buf.len() {
        return Decoded::End;
    }
    let Some(header) = buf.get(at..at + HEADER) else {
        return Decoded::Torn;
    };
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let crc = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if len > MAX_FRAME {
        return Decoded::Torn;
    }
    let start = at + HEADER;
    let Some(body) = buf.get(start..start + len) else {
        return Decoded::Torn;
    };
    if crc32fast::hash(body) != crc {
        return Decoded::Torn;
    }
    Decoded::Frame {
        body,
        next: start + len,
    }
}
