//! Unit tests for `Client` and its `Outbox`.

use popsub_protocol::CloseCode;
use tungstenite::protocol::Message as WsMessage;

use crate::{Client, Outbox, Outgoing, QueueLimits, SendError};

/// Builds a text frame of a given size.
///
/// Args:
///     bytes: Its length.
///
/// Returns:
///     The frame.
fn frame(bytes: usize) -> WsMessage {
    WsMessage::text("x".repeat(bytes))
}

/// Small limits for tests: 10 bytes soft, 30 bytes hard.
const LIMITS: QueueLimits = QueueLimits {
    soft_bytes: 10,
    hard_bytes: 30,
};

/// Checks that a new client gets a non-empty id and no session.
#[test]
fn test_client_new() {
    let (outbox, _rx) = Outbox::channel(QueueLimits::default());
    let client = Client::new(outbox);
    assert!(!client.id.is_empty());
    assert!(client.session.is_none());
}

/// Checks the default limits and that the soft limit follows the hard one.
#[test]
fn limits_derive_the_soft_limit() {
    let defaults = QueueLimits::default();
    assert_eq!(defaults.hard_bytes, 16 * 1024 * 1024);
    assert_eq!(defaults.soft_bytes, 1024 * 1024);
    let tiny = QueueLimits::from_hard_limit(1000);
    assert_eq!((tiny.soft_bytes, tiny.hard_bytes), (1000, 1000));
}

/// Checks that queued bytes are counted, the soft limit turns `has_room`
/// off, and handing frames to the send loop turns it back on.
#[test]
fn the_soft_limit_tracks_queued_bytes() {
    let (outbox, mut rx) = Outbox::channel(LIMITS);
    assert!(outbox.has_room());
    outbox.send(frame(8)).expect("send");
    assert!(outbox.has_room());
    outbox.send(frame(8)).expect("send");
    assert_eq!(outbox.queued_bytes(), 16);
    assert!(!outbox.has_room());
    assert!(matches!(rx.try_recv(), Some(Outgoing::Frame(_))));
    assert_eq!(outbox.queued_bytes(), 8);
    assert!(outbox.has_room());
}

/// Checks that going over the hard limit refuses the frame, closes the
/// connection as a slow consumer, and drops the backlog.
#[test]
fn the_hard_limit_closes_as_a_slow_consumer() {
    let (outbox, mut rx) = Outbox::channel(LIMITS);
    outbox.send(frame(20)).expect("send");
    assert_eq!(outbox.send(frame(20)), Err(SendError::Overflow));
    assert_eq!(outbox.closing(), Some(CloseCode::SlowConsumer));
    assert_eq!(outbox.send(frame(1)), Err(SendError::Closed));
    assert_eq!(
        rx.try_recv(),
        Some(Outgoing::Close(CloseCode::SlowConsumer))
    );
}

/// Checks that an ordinary close still delivers frames queued before it,
/// and that the first reason wins.
#[test]
fn queued_frames_go_out_before_an_ordinary_close() {
    let (outbox, mut rx) = Outbox::channel(LIMITS);
    outbox.send(frame(3)).expect("send");
    outbox.close(CloseCode::AuthFailed);
    outbox.close(CloseCode::GoingAway);
    assert!(matches!(rx.try_recv(), Some(Outgoing::Frame(_))));
    assert_eq!(rx.try_recv(), Some(Outgoing::Close(CloseCode::AuthFailed)));
}

/// Checks that `ready` keeps waiting while the queue is under the soft limit
/// but above half of it, and wakes once it reaches half.
#[tokio::test]
async fn ready_waits_for_half_the_soft_limit() {
    let (outbox, mut rx) = Outbox::channel(LIMITS);
    for _ in 0..3 {
        outbox.send(frame(4)).expect("send");
    }
    let waiter = {
        let outbox = outbox.clone();
        tokio::spawn(async move { outbox.ready().await })
    };
    assert!(matches!(rx.recv().await, Some(Outgoing::Frame(_))));
    assert!(outbox.has_room());
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    assert!(matches!(rx.recv().await, Some(Outgoing::Frame(_))));
    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("ready woke up")
        .expect("task");
}

/// Checks that `ready` waits until the queue drains below the soft limit,
/// and that `closed` and `recv` wake on a close.
#[tokio::test]
async fn waiters_wake_on_drain_and_close() {
    let (outbox, mut rx) = Outbox::channel(LIMITS);
    outbox.send(frame(20)).expect("send");
    let waiter = {
        let outbox = outbox.clone();
        tokio::spawn(async move { outbox.ready().await })
    };
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    assert!(matches!(rx.recv().await, Some(Outgoing::Frame(_))));
    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("ready woke up")
        .expect("task");

    let closed = {
        let outbox = outbox.clone();
        tokio::spawn(async move { outbox.closed().await })
    };
    let next = tokio::spawn(async move { rx.recv().await });
    tokio::task::yield_now().await;
    outbox.close(CloseCode::IdleTimeout);
    assert_eq!(closed.await.expect("task"), CloseCode::IdleTimeout);
    assert_eq!(
        next.await.expect("task"),
        Some(Outgoing::Close(CloseCode::IdleTimeout))
    );
}

/// Checks that the receiver ends without a close once every outbox is dropped.
#[tokio::test]
async fn dropping_every_outbox_ends_the_receiver() {
    let (outbox, mut rx) = Outbox::channel(LIMITS);
    drop(outbox);
    assert_eq!(rx.recv().await, None);
}
