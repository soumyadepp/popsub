//! `Outbox`, a connection's outgoing queue with a byte budget and a close signal.
//!
//! Everything sent to one connection (replies, deliveries, pings) goes
//! through its `Outbox`, and the transport's send loop drains the matching
//! `OutboxReceiver` into the socket. The outbox counts the bytes waiting:
//!
//! - Above the soft limit, `has_room` is false. The broker stops putting new
//!   deliveries in the queue and the subscription falls behind; once the
//!   queue drains (`ready`), it is caught up from the topic log. Memory
//!   stays bounded without dropping anything.
//! - A send that would go over the hard limit is refused and the connection
//!   is closed with `CloseCode::SlowConsumer`. Only frames that ignore the
//!   soft limit (replies, retries, pings) can get that far.
//!
//! Closing goes through the outbox too (`close`), so there is one way to
//! end a connection. Frames already queued are still sent before the close
//! frame, except for a slow consumer, whose backlog is dropped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use popsub_protocol::CloseCode;
use tokio::sync::Notify;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, error::TryRecvError};
use tungstenite::protocol::Message as WsMessage;

/// The byte budget of one connection's outgoing queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    /// Above this many queued bytes, no new deliveries are queued; the
    /// subscription is caught up from the log once the queue drains.
    pub soft_bytes: usize,
    /// A send that would go over this many queued bytes closes the
    /// connection as a slow consumer.
    pub hard_bytes: usize,
}

impl QueueLimits {
    /// Builds limits from a hard limit, with the soft limit a sixteenth of it
    /// (and at least 64 KiB, and never above the hard limit).
    ///
    /// Args:
    ///     hard_bytes: The most bytes a connection may have queued.
    ///
    /// Returns:
    ///     The limits.
    pub fn from_hard_limit(hard_bytes: usize) -> Self {
        let hard_bytes = hard_bytes.max(1);
        Self {
            soft_bytes: (hard_bytes / 16).max(64 * 1024).min(hard_bytes),
            hard_bytes,
        }
    }
}

impl Default for QueueLimits {
    /// Builds the default limits: 16 MiB hard, 1 MiB soft.
    ///
    /// Returns:
    ///     The default limits.
    fn default() -> Self {
        Self::from_hard_limit(16 * 1024 * 1024)
    }
}

/// Why a send was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The connection is closing or gone.
    Closed,
    /// The frame would have gone over the hard limit; the connection is now
    /// closing as a slow consumer.
    Overflow,
}

/// What the send loop should write next.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// A queued frame.
    Frame(WsMessage),
    /// A close frame with this code; nothing more follows.
    Close(CloseCode),
}

/// The state an outbox and its receiver share.
#[derive(Debug)]
struct Shared {
    /// Bytes queued and not yet handed to the send loop.
    queued: AtomicUsize,
    /// The byte budget.
    limits: QueueLimits,
    /// Why the connection is closing, once it is. The first reason wins.
    closing: Mutex<Option<CloseCode>>,
    /// Woken when the queue drains or the connection starts closing.
    changed: Notify,
}

impl Shared {
    /// Reads the closing reason.
    ///
    /// Returns:
    ///     The reason, or `None` while the connection is open.
    fn closing(&self) -> Option<CloseCode> {
        *self.closing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The sending side of a connection's outgoing queue. Cloning shares it.
#[derive(Debug, Clone)]
pub struct Outbox {
    /// The channel to the send loop.
    tx: UnboundedSender<WsMessage>,
    /// Byte count, limits and closing state.
    shared: Arc<Shared>,
}

/// The receiving side, drained by the connection's send loop.
#[derive(Debug)]
pub struct OutboxReceiver {
    /// The channel from the outbox.
    rx: UnboundedReceiver<WsMessage>,
    /// Byte count, limits and closing state.
    shared: Arc<Shared>,
}

impl Outbox {
    /// Creates a connection's outgoing queue.
    ///
    /// Args:
    ///     limits: The byte budget.
    ///
    /// Returns:
    ///     The outbox and the receiver for the send loop.
    pub fn channel(limits: QueueLimits) -> (Outbox, OutboxReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            queued: AtomicUsize::new(0),
            limits,
            closing: Mutex::new(None),
            changed: Notify::new(),
        });
        (
            Outbox {
                tx,
                shared: shared.clone(),
            },
            OutboxReceiver { rx, shared },
        )
    }

    /// Queues a frame, unless the connection is closing or the frame would
    /// go over the hard limit. Ignores the soft limit; callers that can wait
    /// check `has_room` first.
    ///
    /// Args:
    ///     frame: The frame.
    ///
    /// Raises:
    ///     SendError::Closed: When the connection is closing or gone.
    ///     SendError::Overflow: When the frame would go over the hard limit.
    ///         The connection is then closed with `CloseCode::SlowConsumer`.
    pub fn send(&self, frame: WsMessage) -> Result<(), SendError> {
        if self.shared.closing().is_some() {
            return Err(SendError::Closed);
        }
        let len = frame.len();
        let total = self.shared.queued.fetch_add(len, Ordering::SeqCst) + len;
        if total > self.shared.limits.hard_bytes {
            self.shared.queued.fetch_sub(len, Ordering::SeqCst);
            self.close(CloseCode::SlowConsumer);
            return Err(SendError::Overflow);
        }
        self.tx.send(frame).map_err(|_| {
            self.shared.queued.fetch_sub(len, Ordering::SeqCst);
            SendError::Closed
        })
    }

    /// Whether new deliveries may be queued, that is, the queue is at or
    /// below the soft limit and the connection isn't closing.
    ///
    /// Returns:
    ///     True when there is room.
    pub fn has_room(&self) -> bool {
        self.shared.closing().is_none()
            && self.shared.queued.load(Ordering::SeqCst) <= self.shared.limits.soft_bytes
    }

    /// How many bytes are queued.
    ///
    /// Returns:
    ///     The byte count.
    pub fn queued_bytes(&self) -> usize {
        self.shared.queued.load(Ordering::SeqCst)
    }

    /// Waits until there is room again, or the connection starts closing.
    pub async fn ready(&self) {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.has_room() || self.shared.closing().is_some() {
                return;
            }
            changed.await;
        }
    }

    /// Starts closing the connection. Only the first reason is kept.
    ///
    /// Args:
    ///     code: Why it is closing.
    pub fn close(&self, code: CloseCode) {
        {
            let mut closing = self
                .shared
                .closing
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if closing.is_some() {
                return;
            }
            *closing = Some(code);
        }
        self.shared.changed.notify_waiters();
    }

    /// Why the connection is closing.
    ///
    /// Returns:
    ///     The reason, or `None` while it is open.
    pub fn closing(&self) -> Option<CloseCode> {
        self.shared.closing()
    }

    /// Waits until the connection starts closing.
    ///
    /// Returns:
    ///     Why it is closing.
    pub async fn closed(&self) -> CloseCode {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(code) = self.shared.closing() {
                return code;
            }
            changed.await;
        }
    }
}

impl OutboxReceiver {
    /// Hands over one frame and takes its bytes off the count.
    ///
    /// Args:
    ///     frame: The frame leaving the queue.
    ///
    /// Returns:
    ///     The frame, wrapped for the send loop.
    fn take(&self, frame: WsMessage) -> Outgoing {
        self.shared.queued.fetch_sub(frame.len(), Ordering::SeqCst);
        self.shared.changed.notify_waiters();
        Outgoing::Frame(frame)
    }

    /// Returns what the send loop should write next, without waiting.
    ///
    /// Queued frames come first, then the close, except for a slow consumer,
    /// whose close comes straight away and whose backlog is dropped.
    ///
    /// Returns:
    ///     The next frame or close, or `None` when there is nothing yet (or
    ///     the outbox is gone without closing).
    pub fn try_recv(&mut self) -> Option<Outgoing> {
        let closing = self.shared.closing();
        if closing == Some(CloseCode::SlowConsumer) {
            return Some(Outgoing::Close(CloseCode::SlowConsumer));
        }
        match self.rx.try_recv() {
            Ok(frame) => Some(self.take(frame)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => closing.map(Outgoing::Close),
        }
    }

    /// Waits for what the send loop should write next.
    ///
    /// Returns:
    ///     The next frame or close, or `None` once every outbox is dropped
    ///     without a close.
    pub async fn recv(&mut self) -> Option<Outgoing> {
        let shared = self.shared.clone();
        loop {
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(next) = self.try_recv() {
                return Some(next);
            }
            tokio::select! {
                frame = self.rx.recv() => match frame {
                    Some(frame) => {
                        if self.shared.closing() == Some(CloseCode::SlowConsumer) {
                            return Some(Outgoing::Close(CloseCode::SlowConsumer));
                        }
                        return Some(self.take(frame));
                    }
                    None => return self.shared.closing().map(Outgoing::Close),
                },
                _ = &mut changed => {}
            }
        }
    }
}
