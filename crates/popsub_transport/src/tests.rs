//! Tests for the JSON wire format of client and server frames.
//!
//! Clients in other languages build these frames by hand, so the exact field
//! names and `type` tags are part of the protocol. Server behaviour is covered
//! by `websocket_tests.rs`, which drives a real server.

use crate::message::{ClientMessage, ServerMessage};
use popsub_broker::{Delivery, StartFrom, TopicFrame, Truncated};
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

/// Checks that `subscribe` without `from` means no replay, and that each
/// `from` form parses.
#[test]
fn subscribe_from_forms_parse() {
    let cases = [
        (
            json!({"type": "subscribe", "topic": "chat"}),
            StartFrom::New,
        ),
        (
            json!({"type": "subscribe", "topic": "chat", "from": "new"}),
            StartFrom::New,
        ),
        (
            json!({"type": "subscribe", "topic": "chat", "from": {"last": 50}}),
            StartFrom::Last(50),
        ),
        (
            json!({"type": "subscribe", "topic": "chat", "from": {"since_ms": 1794800000000_i64}}),
            StartFrom::SinceMs(1_794_800_000_000),
        ),
        (
            json!({"type": "subscribe", "topic": "chat", "from": {"seq": 1200}}),
            StartFrom::Seq(1200),
        ),
    ];
    for (value, expected) in cases {
        match parse(value.clone()) {
            ClientMessage::Subscribe { topic, from } => {
                assert_eq!(topic, "chat");
                assert_eq!(from, expected, "{value}");
            }
            other => panic!("expected subscribe, got {other:?}"),
        }
    }
    assert!(
        serde_json::from_value::<ClientMessage>(
            json!({"type": "subscribe", "topic": "chat", "from": {"last": "all"}})
        )
        .is_err()
    );
}

/// Builds the delivery used by the frame tests.
///
/// Returns:
///     A QoS 1 delivery with seq 42.
fn delivery() -> Delivery {
    Delivery {
        topic: "chat".to_string(),
        seq: 42,
        message_id: "m-1".to_string(),
        payload: "hi".to_string(),
        qos: 1,
        published_at_ms: 1_794_800_000_000,
    }
}

/// Checks the exact JSON of delivered records and truncation notices.
#[test]
fn topic_frames_serialise_with_type_tags() {
    assert_eq!(
        serde_json::to_value(ServerMessage::Message(delivery())).unwrap(),
        json!({
            "type": "message",
            "topic": "chat",
            "seq": 42,
            "message_id": "m-1",
            "payload": "hi",
            "qos": 1,
            "published_at_ms": 1794800000000_i64
        })
    );
    assert_eq!(
        serde_json::to_value(ServerMessage::Truncated(Truncated {
            topic: "chat".to_string(),
            first_available: 1300,
        }))
        .unwrap(),
        json!({"type": "truncated", "topic": "chat", "first_available": 1300})
    );
}

/// Checks that what the broker writes parses as the transport's
/// `ServerMessage`, so the two definitions can't drift apart.
#[test]
fn broker_frames_parse_as_server_messages() {
    let message = TopicFrame::Message(delivery());
    let parsed: ServerMessage = serde_json::to_value(&message)
        .and_then(serde_json::from_value)
        .unwrap();
    assert!(matches!(parsed, ServerMessage::Message(d) if d == delivery()));

    let truncated = TopicFrame::Truncated(Truncated {
        topic: "chat".to_string(),
        first_available: 7,
    });
    let parsed: ServerMessage = serde_json::to_value(&truncated)
        .and_then(serde_json::from_value)
        .unwrap();
    assert!(matches!(parsed, ServerMessage::Truncated(t) if t.first_available == 7));
}
