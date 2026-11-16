//! Tests for the exact JSON of every frame.
//!
//! Clients in other languages build and parse these by hand, so field names,
//! `type` tags and where the `id` goes are the protocol itself.

use serde_json::{Value, json};

use crate::{
    ClientFrame, ClientMessage, Delivery, ErrorCode, Limits, RequestId, ServerFrame, ServerMessage,
    Stalled, StartFrom, Subscribed, Truncated,
};

/// Parses a client frame, failing the test if it doesn't parse.
///
/// Args:
///     value: The frame as a client would send it.
///
/// Returns:
///     The parsed frame.
fn parse(value: Value) -> ClientFrame {
    serde_json::from_value(value).expect("frame should parse")
}

/// Serialises a server frame to JSON.
///
/// Args:
///     frame: The frame.
///
/// Returns:
///     Its JSON.
fn json_of(frame: ServerFrame) -> Value {
    serde_json::to_value(frame).expect("frame serialises")
}

/// Checks that the `id` sits beside `type`, may be a number or a string, and
/// is optional.
#[test]
fn request_ids_are_optional_numbers_or_strings() {
    let numbered = parse(json!({"type": "unsubscribe", "id": 7, "topic": "chat"}));
    assert_eq!(numbered.id, Some(RequestId::Number(7)));
    let named = parse(json!({"type": "unsubscribe", "id": "a-1", "topic": "chat"}));
    assert_eq!(named.id, Some(RequestId::Text("a-1".to_string())));
    let bare = parse(json!({"type": "unsubscribe", "topic": "chat"}));
    assert_eq!(bare.id, None);
    assert_eq!(
        bare.message,
        ClientMessage::Unsubscribe {
            topic: "chat".to_string()
        }
    );
}

/// Checks that each client request parses from its documented form.
#[test]
fn client_requests_parse() {
    let cases = [
        (
            json!({"type": "login", "username": "alice", "password": "pw"}),
            ClientMessage::Login {
                username: "alice".to_string(),
                password: "pw".to_string(),
            },
        ),
        (
            json!({"type": "auth", "token": "t"}),
            ClientMessage::Auth {
                token: "t".to_string(),
                protocol: None,
                session_id: None,
                clean_start: false,
            },
        ),
        (
            json!({"type": "auth", "token": "t", "protocol": 1, "session_id": "phone", "clean_start": true}),
            ClientMessage::Auth {
                token: "t".to_string(),
                protocol: Some(1),
                session_id: Some("phone".to_string()),
                clean_start: true,
            },
        ),
        (
            json!({"type": "publish", "topic": "chat", "payload": "hi"}),
            ClientMessage::Publish {
                topic: "chat".to_string(),
                payload: "hi".to_string(),
                message_id: None,
                qos: None,
            },
        ),
        (
            json!({"type": "ack", "topic": "chat", "seq": 42}),
            ClientMessage::Ack {
                topic: "chat".to_string(),
                seq: 42,
            },
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(parse(value.clone()).message, expected, "{value}");
    }
}

/// Checks every form of `from`, and that a subscribe without it means `resume`.
#[test]
fn subscribe_from_forms_parse() {
    let cases = [
        (json!(null), StartFrom::Resume),
        (json!("resume"), StartFrom::Resume),
        (json!("new"), StartFrom::New),
        (json!({"last": 50}), StartFrom::Last(50)),
        (
            json!({"since_ms": 1794800000000_i64}),
            StartFrom::SinceMs(1_794_800_000_000),
        ),
        (json!({"seq": 1200}), StartFrom::Seq(1200)),
    ];
    for (from, expected) in cases {
        let mut value = json!({"type": "subscribe", "topic": "chat"});
        if !from.is_null() {
            value["from"] = from.clone();
        }
        match parse(value).message {
            ClientMessage::Subscribe { from: parsed, .. } => assert_eq!(parsed, expected, "{from}"),
            other => panic!("expected subscribe, got {other:?}"),
        }
    }
}

/// Checks that unknown fields are ignored, so newer clients can talk to older servers.
#[test]
fn unknown_fields_are_ignored() {
    let frame = parse(json!({"type": "unsubscribe", "topic": "chat", "added_later": true}));
    assert_eq!(
        frame.message,
        ClientMessage::Unsubscribe {
            topic: "chat".to_string()
        }
    );
}

/// Checks that unknown types, missing fields, wrong types and the old ack
/// shape are rejected.
#[test]
fn malformed_frames_are_rejected() {
    for value in [
        json!({"type": "teleport", "topic": "chat"}),
        json!({"type": "subscribe"}),
        json!({"topic": "chat"}),
        json!({"type": "ack", "message_id": "m"}),
        json!({"type": "subscribe", "topic": "chat", "from": {"last": "all"}}),
        json!({"type": "unsubscribe", "topic": "chat", "id": [1]}),
    ] {
        assert!(
            serde_json::from_value::<ClientFrame>(value.clone()).is_err(),
            "{value}"
        );
    }
}

/// Checks the exact JSON of replies, with and without an `id`.
#[test]
fn replies_serialise_with_type_and_id() {
    let id = Some(RequestId::Number(3));
    let cases = [
        (
            ServerFrame::reply(
                id.clone(),
                ServerMessage::LoggedIn {
                    token: "t".to_string(),
                },
            ),
            json!({"type": "logged_in", "id": 3, "token": "t"}),
        ),
        (
            ServerFrame::reply(None, ServerMessage::Registered {}),
            json!({"type": "registered"}),
        ),
        (
            ServerFrame::reply(
                id.clone(),
                ServerMessage::Authenticated {
                    protocol: 1,
                    session_id: None,
                    resumed: Vec::new(),
                    limits: Limits {
                        max_inflight: 100,
                        session_expiry_secs: 86_400,
                    },
                },
            ),
            json!({"type": "authenticated", "id": 3, "protocol": 1, "limits": {"max_inflight": 100, "session_expiry_secs": 86400}}),
        ),
        (
            ServerFrame::reply(
                Some(RequestId::Text("s".to_string())),
                ServerMessage::Authenticated {
                    protocol: 1,
                    session_id: Some("phone".to_string()),
                    resumed: vec!["chat".to_string()],
                    limits: Limits {
                        max_inflight: 5,
                        session_expiry_secs: 60,
                    },
                },
            ),
            json!({"type": "authenticated", "id": "s", "protocol": 1, "session_id": "phone", "resumed": ["chat"], "limits": {"max_inflight": 5, "session_expiry_secs": 60}}),
        ),
        (
            ServerFrame::reply(
                id.clone(),
                ServerMessage::Subscribed(Subscribed {
                    topic: "chat".to_string(),
                    replay_from: 4,
                    replay_to: 9,
                }),
            ),
            json!({"type": "subscribed", "id": 3, "topic": "chat", "replay_from": 4, "replay_to": 9}),
        ),
        (
            ServerFrame::reply(
                id.clone(),
                ServerMessage::Unsubscribed {
                    topic: "chat".to_string(),
                },
            ),
            json!({"type": "unsubscribed", "id": 3, "topic": "chat"}),
        ),
        (
            ServerFrame::reply(
                id.clone(),
                ServerMessage::Published {
                    message_id: "m-1".to_string(),
                    seq: 7,
                },
            ),
            json!({"type": "published", "id": 3, "message_id": "m-1", "seq": 7}),
        ),
        (
            ServerFrame::reply(
                id,
                ServerMessage::Error {
                    code: ErrorCode::RateLimited,
                    message: "slow down".to_string(),
                    retry_after: Some(2),
                },
            ),
            json!({"type": "error", "id": 3, "code": "rate_limited", "message": "slow down", "retry_after": 2}),
        ),
        (
            ServerFrame::reply(
                None,
                ServerMessage::Error {
                    code: ErrorCode::NotAuthorized,
                    message: "no".to_string(),
                    retry_after: None,
                },
            ),
            json!({"type": "error", "code": "not_authorized", "message": "no"}),
        ),
    ];
    for (frame, expected) in cases {
        assert_eq!(json_of(frame), expected);
    }
}

/// Checks the exact JSON of frames the server sends on its own.
#[test]
fn events_serialise_without_an_id() {
    let cases = [
        (
            ServerMessage::Message(Delivery {
                topic: "chat".to_string(),
                seq: 42,
                message_id: "m-1".to_string(),
                payload: "hi".to_string(),
                qos: 1,
                published_at_ms: 1_794_800_000_000,
            }),
            json!({"type": "message", "topic": "chat", "seq": 42, "message_id": "m-1", "payload": "hi", "qos": 1, "published_at_ms": 1794800000000_i64}),
        ),
        (
            ServerMessage::Truncated(Truncated {
                topic: "chat".to_string(),
                first_available: 1300,
            }),
            json!({"type": "truncated", "topic": "chat", "first_available": 1300}),
        ),
        (
            ServerMessage::Stalled(Stalled {
                topic: "chat".to_string(),
                seq: 5,
            }),
            json!({"type": "stalled", "topic": "chat", "seq": 5}),
        ),
    ];
    for (message, expected) in cases {
        assert_eq!(json_of(ServerFrame::event(message)), expected);
    }
}

/// Checks that every server frame parses back to itself, so clients can use these types.
#[test]
fn server_frames_round_trip() {
    let frames = [
        ServerFrame::reply(
            Some(RequestId::Number(1)),
            ServerMessage::Published {
                message_id: "m".to_string(),
                seq: 1,
            },
        ),
        ServerFrame::event(ServerMessage::Stalled(Stalled {
            topic: "t".to_string(),
            seq: 2,
        })),
        ServerFrame::reply(
            None,
            ServerMessage::Error {
                code: ErrorCode::StorageUnavailable,
                message: "later".to_string(),
                retry_after: None,
            },
        ),
    ];
    for frame in frames {
        let text = serde_json::to_string(&frame).expect("serialise");
        let parsed: ServerFrame = serde_json::from_str(&text).expect("parse");
        assert_eq!(parsed, frame);
    }
}

/// Checks that every error code has the documented snake_case name.
#[test]
fn error_codes_are_snake_case() {
    let names = [
        (ErrorCode::BadRequest, "bad_request"),
        (ErrorCode::Unauthenticated, "unauthenticated"),
        (ErrorCode::AuthFailed, "auth_failed"),
        (ErrorCode::InvalidCredentials, "invalid_credentials"),
        (ErrorCode::LockedOut, "locked_out"),
        (ErrorCode::RateLimited, "rate_limited"),
        (ErrorCode::NotAuthorized, "not_authorized"),
        (ErrorCode::RegistrationFailed, "registration_failed"),
        (ErrorCode::InvalidSessionId, "invalid_session_id"),
        (ErrorCode::SessionUnavailable, "session_unavailable"),
        (ErrorCode::UnsupportedProtocol, "unsupported_protocol"),
        (ErrorCode::StorageUnavailable, "storage_unavailable"),
        (ErrorCode::Internal, "internal"),
    ];
    for (code, name) in names {
        assert_eq!(serde_json::to_value(code).unwrap(), json!(name));
    }
}
