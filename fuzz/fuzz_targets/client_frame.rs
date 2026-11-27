//! Fuzzes the parser every WebSocket text frame from a client goes through.
//!
//! Each input is read the way the server reads it: `parse_frame`, which tries a
//! direct `publish` shape before the general parser, and, when that fails,
//! `salvage_request_id`, which looks for an `id` to put on the `bad_request`
//! reply. Neither may panic, and `parse_frame` must agree with the general
//! `ClientFrame` parser on every input: the same frame, or an error from both.

#![no_main]

use libfuzzer_sys::fuzz_target;
use popsub_protocol::ClientFrame;
use popsub_transport::websocket::{parse_frame, salvage_request_id};

/// Parses one input as a client frame and checks the two parsers agree.
///
/// Inputs that aren't UTF-8 are skipped, since a WebSocket text frame that
/// isn't UTF-8 is refused before it reaches the parser.
///
/// Args:
///     data: The fuzzer's input.
fn check(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let general = serde_json::from_str::<ClientFrame>(text);
    let fast = parse_frame(text);
    match (general, fast) {
        (Ok(general), Ok(fast)) => assert_eq!(general, fast, "the parsers disagree on {text:?}"),
        (Err(_), Err(_)) => {
            salvage_request_id(text);
        }
        (general, fast) => {
            panic!("the parsers disagree on {text:?}: general {general:?}, fast {fast:?}")
        }
    }
}

fuzz_target!(|data: &[u8]| check(data));
