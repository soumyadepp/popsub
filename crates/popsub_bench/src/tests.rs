//! Unit tests for the parts of the benchmark that don't need a server: frame templates, the
//! measurement window, CPU time parsing and the report's formatting.

use popsub_protocol::{ClientFrame, ClientMessage, RequestId};

use crate::load::{STAMP_DIGITS, Window, stamp, stamp_of};
use crate::mqtt::{publish_parts, push_remaining_length};
use crate::nats::{message_length, subject};
use crate::popsub::PublishTemplate;
use crate::report::thousands;
use crate::scenario::scenarios;
use crate::server::{parse_cputime, parse_proc_stat};

/// A rendered publish is a frame the protocol crate reads back with the same fields.
#[test]
fn a_rendered_publish_is_a_valid_protocol_frame() {
    let template = PublishTemplate::new("bench/a \"quoted\" topic", 1, 32);
    let frame: ClientFrame = serde_json::from_str(&template.render(0xabc, Some(7))).unwrap();
    assert_eq!(frame.id, Some(RequestId::Number(7)));
    let ClientMessage::Publish {
        topic,
        payload,
        qos,
        ..
    } = frame.message
    else {
        panic!("expected a publish");
    };
    assert_eq!(topic, "bench/a \"quoted\" topic");
    assert_eq!(qos, Some(1));
    assert_eq!(payload.len(), 32);
    assert_eq!(stamp_of(payload.as_bytes()), Some(0xabc));
}

/// A publish without an id has none on the wire, and a short payload still holds its stamp.
#[test]
fn a_publish_without_an_id_keeps_its_stamp_in_a_short_payload() {
    let template = PublishTemplate::new("bench/t", 0, 4);
    let frame: ClientFrame = serde_json::from_str(&template.render(u64::MAX, None)).unwrap();
    assert_eq!(frame.id, None);
    let ClientMessage::Publish { payload, .. } = frame.message else {
        panic!("expected a publish");
    };
    assert_eq!(payload.len(), STAMP_DIGITS);
    assert_eq!(stamp_of(payload.as_bytes()), Some(u64::MAX));
}

/// A payload the benchmark didn't write has no stamp.
#[test]
fn a_foreign_payload_has_no_stamp() {
    assert_eq!(stamp_of(b"hello"), None);
    assert_eq!(stamp_of(b"not sixteen hex!!"), None);
    assert_eq!(stamp_of(stamp(42).as_bytes()), Some(42));
}

/// The window includes its start and excludes its end.
#[test]
fn the_window_is_half_open() {
    let window = Window {
        begin_ns: 10,
        end_ns: 20,
    };
    assert!(!window.contains(9));
    assert!(window.contains(10));
    assert!(window.contains(19));
    assert!(!window.contains(20));
    assert!((window.seconds() - 1e-8).abs() < 1e-12);
}

/// `ps` CPU times parse in the macOS and Linux formats, with and without days.
#[test]
fn cputime_parses_macos_and_linux_formats() {
    assert_eq!(parse_cputime("0:01.25"), Some(1.25));
    assert_eq!(parse_cputime("12:34.50"), Some(754.5));
    assert_eq!(parse_cputime("01:02:03"), Some(3723.0));
    assert_eq!(parse_cputime("2-01:02:03"), Some(176_523.0));
    assert_eq!(parse_cputime(""), None);
    assert_eq!(parse_cputime("abc"), None);
}

/// `/proc/<pid>/stat` yields user plus system time even when the process name has spaces.
#[test]
fn proc_stat_adds_user_and_system_time() {
    let stat = "42 (pop sub) S 1 42 42 0 -1 4194304 100 0 0 0 250 50 0 0 20 0 8 0 100 0 0";
    assert_eq!(parse_proc_stat(stat), Some(3.0));
    assert_eq!(parse_proc_stat("garbage"), None);
}

/// Counts are grouped in threes.
#[test]
fn thousands_groups_digits() {
    assert_eq!(thousands(0.0), "0");
    assert_eq!(thousands(999.4), "999");
    assert_eq!(thousands(1_000.0), "1,000");
    assert_eq!(thousands(1_234_567.0), "1,234,567");
}

/// Scenario names are unique, since `--scenario` selects by name.
#[test]
fn scenario_names_are_unique() {
    let mut names: Vec<&str> = scenarios().iter().map(|s| s.name).collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count);
}

/// MQTT's remaining length takes one byte up to 127 and continues in seven-bit groups after.
#[test]
fn mqtt_remaining_length_uses_seven_bit_groups() {
    let encode = |length| {
        let mut packet = Vec::new();
        push_remaining_length(&mut packet, length);
        packet
    };
    assert_eq!(encode(0), [0x00]);
    assert_eq!(encode(127), [0x7f]);
    assert_eq!(encode(128), [0x80, 0x01]);
    assert_eq!(encode(16_383), [0xff, 0x7f]);
    assert_eq!(encode(16_384), [0x80, 0x80, 0x01]);
}

/// An MQTT publish splits into a packet id, only at QoS 1, and its payload.
#[test]
fn an_mqtt_publish_splits_into_id_and_payload() {
    let qos0 = [0x00, 0x03, b'a', b'/', b'b', b'h', b'i'];
    assert_eq!(publish_parts(0x30, &qos0), Some((None, &b"hi"[..])));
    let qos1 = [0x00, 0x03, b'a', b'/', b'b', 0x01, 0x02, b'h', b'i'];
    assert_eq!(publish_parts(0x32, &qos1), Some((Some(0x0102), &b"hi"[..])));
    assert_eq!(publish_parts(0x32, &[0x00, 0x03, b'a']), None);
}

/// A NATS message line ends with its payload length, and topics become dotted subjects.
#[test]
fn nats_lines_and_subjects_are_read_as_the_protocol_writes_them() {
    assert_eq!(message_length(b"MSG bench.a 1 128\r\n"), Some(128));
    assert_eq!(message_length(b"MSG bench.a 1 _INBOX.x 7\r\n"), Some(7));
    assert_eq!(message_length(b"MSG bench.a 1 abc\r\n"), None);
    assert_eq!(subject("bench/one-topic/all/0"), "bench.one-topic.all.0");
}
