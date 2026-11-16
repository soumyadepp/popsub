//! Tests for the JSON wire format of client and server frames.
//!
//! Clients in other languages build these frames by hand, so the exact field
//! names and `type` tags are part of the protocol. Server behaviour is covered
//! by `websocket_tests.rs`, which drives a real server.

use crate::message::{ClientMessage, ServerMessage};
use serde_json::json;

/// Parses a client frame from a JSON value.
///
/// Args:
///     value: The frame as a client would send it.
///
/// Returns:
///     The parsed frame. Panics if it doesn't parse.
fn parse(value: serde_json::Value) -> ClientMessage {
    serde_json::from_value(value).expect("frame should parse")
}

/// Checks that login and register frames are read from their `type` tag and fields.
#[test]
fn login_and_register_frames_parse() {
    assert!(matches!(
        parse(json!({"type": "login", "username": "alice", "password": "secret"})),
        ClientMessage::Login { username, password } if username == "alice" && password == "secret"
    ));
    assert!(matches!(
        parse(json!({"type": "register", "username": "bob", "password": "secret"})),
        ClientMessage::Register { username, .. } if username == "bob"
    ));
}

/// Checks that a publish frame may leave out `message_id` and `qos`.
#[test]
fn publish_optional_fields_default_to_none() {
    match parse(json!({"type": "publish", "topic": "chat", "payload": "hi"})) {
        ClientMessage::Publish {
            topic,
            payload,
            message_id,
            qos,
        } => {
            assert_eq!((topic.as_str(), payload.as_str()), ("chat", "hi"));
            assert_eq!(message_id, None);
            assert_eq!(qos, None);
        }
        other => panic!("expected publish, got {other:?}"),
    }
}

/// Checks that frames with an unknown `type`, or missing required fields, are rejected.
#[test]
fn malformed_frames_are_rejected() {
    for value in [
        json!({"type": "teleport", "topic": "chat"}),
        json!({"type": "subscribe"}),
        json!({"topic": "chat"}),
    ] {
        assert!(
            serde_json::from_value::<ClientMessage>(value.clone()).is_err(),
            "{value}"
        );
    }
}

/// Checks that server frames carry the `type` tags and fields clients look for.
#[test]
fn server_frames_serialise_with_type_tags() {
    let cases = [
        (
            ServerMessage::LoginResponse {
                token: "t".to_string(),
            },
            json!({"type": "login_response", "token": "t"}),
        ),
        (
            ServerMessage::RegisterResponse {
                success: false,
                message: "registration failed".to_string(),
            },
            json!({"type": "register_response", "success": false, "message": "registration failed"}),
        ),
        (
            ServerMessage::Authenticated {},
            json!({"type": "authenticated"}),
        ),
        (
            ServerMessage::Error {
                message: "must authenticate first".to_string(),
            },
            json!({"type": "error", "message": "must authenticate first"}),
        ),
    ];
    for (frame, expected) in cases {
        assert_eq!(serde_json::to_value(&frame).unwrap(), expected);
    }
}
