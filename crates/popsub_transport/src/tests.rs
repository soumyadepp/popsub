//! Tests for the JSON wire format of client and server frames.
//!
//! Clients in other languages build these frames by hand, so the exact field
//! names and `type` tags are part of the protocol. Server behaviour is covered
//! by `websocket_tests.rs`, which drives a real server.

use crate::message::{ClientMessage, ServerMessage};
use popsub_broker::{Delivery, Stalled, StartFrom, Subscribed, TopicFrame, Truncated};
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
            ServerMessage::Authenticated {
                session_id: None,
                resumed: Vec::new(),
            },
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

/// Checks that `subscribe` without `from` means `resume`, and that each
/// `from` form parses.
#[test]
fn subscribe_from_forms_parse() {
    let cases = [
        (
            json!({"type": "subscribe", "topic": "chat"}),
            StartFrom::Resume,
        ),
        (
            json!({"type": "subscribe", "topic": "chat", "from": "resume"}),
            StartFrom::Resume,
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

/// Checks that `auth` may name a session and ask for a clean start, and that
/// both default to off.
#[test]
fn auth_session_fields_parse() {
    match parse(json!({"type": "auth", "token": "t"})) {
        ClientMessage::Auth {
            session_id,
            clean_start,
            ..
        } => assert_eq!((session_id, clean_start), (None, false)),
        other => panic!("expected auth, got {other:?}"),
    }
    match parse(json!({"type": "auth", "token": "t", "session_id": "phone", "clean_start": true})) {
        ClientMessage::Auth {
            session_id,
            clean_start,
            ..
        } => assert_eq!((session_id.as_deref(), clean_start), (Some("phone"), true)),
        other => panic!("expected auth, got {other:?}"),
    }
}

/// Checks that `ack` names a topic and seq, and that the old message id form is rejected.
#[test]
fn ack_frames_name_topic_and_seq() {
    assert!(matches!(
        parse(json!({"type": "ack", "topic": "chat", "seq": 42})),
        ClientMessage::Ack { topic, seq } if topic == "chat" && seq == 42
    ));
    assert!(
        serde_json::from_value::<ClientMessage>(json!({"type": "ack", "message_id": "m"})).is_err()
    );
}

/// Checks the exact JSON of the session, publish, subscribe and stall frames.
#[test]
fn session_and_delivery_frames_serialise_with_type_tags() {
    let cases = [
        (
            ServerMessage::Authenticated {
                session_id: Some("phone".to_string()),
                resumed: vec!["chat".to_string()],
            },
            json!({"type": "authenticated", "session_id": "phone", "resumed": ["chat"]}),
        ),
        (
            ServerMessage::Published {
                message_id: "m-1".to_string(),
                seq: 7,
            },
            json!({"type": "published", "message_id": "m-1", "seq": 7}),
        ),
        (
            ServerMessage::Subscribed(Subscribed {
                topic: "chat".to_string(),
                replay_from: 4,
                replay_to: 9,
            }),
            json!({"type": "subscribed", "topic": "chat", "replay_from": 4, "replay_to": 9}),
        ),
        (
            ServerMessage::Stalled(Stalled {
                topic: "chat".to_string(),
                seq: 5,
            }),
            json!({"type": "stalled", "topic": "chat", "seq": 5}),
        ),
    ];
    for (frame, expected) in cases {
        assert_eq!(serde_json::to_value(&frame).unwrap(), expected);
    }
}

/// Checks that the broker's subscribe and stall frames parse as `ServerMessage`.
#[test]
fn broker_session_frames_parse_as_server_messages() {
    let subscribed = TopicFrame::Subscribed(Subscribed {
        topic: "chat".to_string(),
        replay_from: 1,
        replay_to: 0,
    });
    let parsed: ServerMessage = serde_json::to_value(&subscribed)
        .and_then(serde_json::from_value)
        .unwrap();
    assert!(matches!(parsed, ServerMessage::Subscribed(s) if s.replay_from == 1));
    let stalled = TopicFrame::Stalled(Stalled {
        topic: "chat".to_string(),
        seq: 3,
    });
    let parsed: ServerMessage = serde_json::to_value(&stalled)
        .and_then(serde_json::from_value)
        .unwrap();
    assert!(matches!(parsed, ServerMessage::Stalled(s) if s.seq == 3));
}
