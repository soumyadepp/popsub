//! `PublishSink`: what a client connection gives the broker so that topic
//! writers can finish that connection's publishes themselves.
//!
//! A connection doesn't wait for each publish to be stored. It queues the
//! record for the topic's writer and reads its next frame. The writer then
//! answers the client directly: `published` for a QoS 1 record, an `error`
//! frame if storage failed. The sink is how the writer reaches the client,
//! and it counts the connection's unfinished publishes so the connection can
//! stop reading when too many are waiting.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use popsub_connection::Outbox;
use popsub_protocol::{ErrorCode, RequestId, ServerFrame, ServerMessage};
use popsub_storage::{Result as StorageResult, StorageError};
use popsub_utils::metrics::{attr, metrics};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::{debug, error};

use crate::frame::to_ws;
use crate::shared::SharedBroker;
use crate::writer::{Command, Published};

/// How many publishes one connection may have waiting for storage. At this
/// many, the connection should stop reading until `PublishSink::has_room`.
///
/// The window is wide so that a connection keeps reading while its topic's
/// writer stores the batch before; a narrow one makes the two take turns.
pub const MAX_IN_FLIGHT_PUBLISHES: usize = 4096;

/// How many payload bytes one connection may have waiting for storage. This
/// is what bounds memory when the payloads are large.
pub const MAX_IN_FLIGHT_BYTES: usize = 8 * 1024 * 1024;

/// The count a full connection waits to drop below before it reads again, so
/// it doesn't stop and start on every publish.
const RESUME_BELOW: usize = MAX_IN_FLIGHT_PUBLISHES / 2;

/// The payload bytes a full connection waits to drop below before it reads again.
const RESUME_BELOW_BYTES: usize = MAX_IN_FLIGHT_BYTES / 2;

/// One connection's way of handing publishes to topic writers.
pub struct PublishSink {
    /// The connection's outgoing queue, where replies are written.
    outbox: Outbox,
    /// The broker, for finding writers and for joining wildcard subscribers
    /// to a topic that just got its first record.
    broker: SharedBroker,
    /// Publishes queued or being stored.
    in_flight: AtomicUsize,
    /// The payload bytes of those publishes.
    in_flight_bytes: AtomicUsize,
    /// Woken when the unfinished publishes drop below the resume levels and
    /// when there are none left.
    changed: Notify,
    /// The topic this connection last published to and its writer's queue, so
    /// a connection that keeps publishing to one topic doesn't look the
    /// writer up under the broker-wide lock every time.
    last_writer: Mutex<Option<(String, mpsc::UnboundedSender<Command>)>>,
}

impl PublishSink {
    /// Creates the sink for one connection.
    ///
    /// Args:
    ///     broker: The broker the connection publishes through.
    ///     outbox: The connection's outgoing queue.
    ///
    /// Returns:
    ///     The sink, shared with every publish the connection queues.
    pub(crate) fn new(broker: SharedBroker, outbox: Outbox) -> Arc<Self> {
        Arc::new(Self {
            outbox,
            broker,
            in_flight: AtomicUsize::new(0),
            in_flight_bytes: AtomicUsize::new(0),
            changed: Notify::new(),
            last_writer: Mutex::new(None),
        })
    }

    /// Whether the connection has as many unfinished publishes as it may.
    ///
    /// Returns:
    ///     True at `MAX_IN_FLIGHT_PUBLISHES` publishes or `MAX_IN_FLIGHT_BYTES`
    ///     of payload, or more.
    pub fn is_full(&self) -> bool {
        self.in_flight.load(Ordering::Acquire) >= MAX_IN_FLIGHT_PUBLISHES
            || self.in_flight_bytes.load(Ordering::Acquire) >= MAX_IN_FLIGHT_BYTES
    }

    /// Waits until a full connection has finished enough publishes to read again.
    pub async fn has_room(&self) {
        self.wait_below(RESUME_BELOW, RESUME_BELOW_BYTES).await;
    }

    /// Waits until every publish the connection queued has finished.
    ///
    /// A connection waits here before handling any request other than a
    /// publish, an acknowledgement or a ping, so that request sees everything
    /// the connection published before it.
    pub async fn drained(&self) {
        self.wait_below(1, usize::MAX).await;
    }

    /// Waits until fewer than `limit` publishes, holding fewer than `bytes`
    /// of payload, are unfinished.
    ///
    /// Args:
    ///     limit: The count to drop below.
    ///     bytes: The payload bytes to drop below.
    async fn wait_below(&self, limit: usize, bytes: usize) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.in_flight.load(Ordering::Acquire) < limit
                && self.in_flight_bytes.load(Ordering::Acquire) < bytes
            {
                return;
            }
            changed.await;
        }
    }

    /// Queues a command on a topic's writer, remembering the writer for the next publish.
    ///
    /// If the remembered writer has stopped, a new one is started.
    ///
    /// Args:
    ///     topic: The topic.
    ///     command: The command to queue.
    pub(crate) fn queue(&self, topic: &str, command: Command) {
        let mut last = self
            .last_writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let command = match last.as_ref() {
            Some((cached, writer)) if cached == topic => match writer.send(command) {
                Ok(()) => return,
                Err(mpsc::error::SendError(command)) => command,
            },
            _ => command,
        };
        let stale = last.as_ref().is_some_and(|(cached, _)| cached == topic);
        let writer = self.broker.writer_for(topic, stale);
        if let Err(mpsc::error::SendError(command)) = writer.send(command) {
            let writer = self.broker.writer_for(topic, true);
            let _ = writer.send(command);
            *last = Some((topic.to_string(), writer));
            return;
        }
        *last = Some((topic.to_string(), writer));
    }

    /// Counts one more unfinished publish.
    ///
    /// Args:
    ///     bytes: The publish's payload size.
    pub(crate) fn begin(&self, bytes: usize) {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.in_flight_bytes.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Tells the client how a publish ended and counts it as finished.
    ///
    /// A stored QoS 1 record gets `published` with its id and `seq`. A stored
    /// QoS 0 record gets nothing. A record that couldn't be stored gets
    /// `storage_unavailable` whatever its QoS. When the record is the
    /// topic's first, wildcard subscribers whose pattern matches are joined
    /// to the topic in the background.
    ///
    /// Args:
    ///     topic: The topic the record was published to.
    ///     id: The publish request's id, echoed in the reply.
    ///     qos: The record's QoS.
    ///     bytes: The publish's payload size, as given to `begin`.
    ///     outcome: What the writer did with the record.
    fn finish(
        &self,
        topic: &str,
        id: Option<RequestId>,
        qos: u8,
        bytes: usize,
        outcome: StorageResult<Published>,
    ) {
        match outcome {
            Ok(published) => {
                debug!(
                    topic,
                    seq = published.seq,
                    duplicate = published.duplicate,
                    "published"
                );
                let first = published.seq == 1 && !published.duplicate;
                if qos == 1 {
                    self.reply(
                        id,
                        ServerMessage::Published {
                            message_id: published.message_id,
                            seq: published.seq,
                        },
                    );
                }
                if first {
                    let broker = self.broker.clone();
                    let topic = topic.to_string();
                    tokio::spawn(async move { broker.join_pattern_holders(&topic).await });
                }
            }
            Err(e) => {
                error!(topic, qos, %e, "publish not stored");
                let code = ErrorCode::StorageUnavailable;
                metrics().errors.add(1, &attr("code", code.as_str()));
                self.reply(
                    id,
                    ServerMessage::Error {
                        code,
                        message: "storage unavailable, nothing was published".to_string(),
                        retry_after: None,
                    },
                );
            }
        }
        let before = self.in_flight.fetch_sub(1, Ordering::AcqRel);
        let bytes_before = self.in_flight_bytes.fetch_sub(bytes, Ordering::AcqRel);
        let bytes_resume =
            bytes_before >= RESUME_BELOW_BYTES && bytes_before - bytes < RESUME_BELOW_BYTES;
        if before == 1 || before == RESUME_BELOW || bytes_resume {
            self.changed.notify_waiters();
        }
    }

    /// Queues a reply on the connection's outgoing queue. A closed or full
    /// queue is ignored: the connection is already going away.
    ///
    /// Args:
    ///     id: The id of the request being answered.
    ///     message: The reply.
    fn reply(&self, id: Option<RequestId>, message: ServerMessage) {
        if let Some(frame) = to_ws(&ServerFrame::reply(id, message)) {
            let _ = self.outbox.send(frame);
        }
    }
}

impl std::fmt::Debug for PublishSink {
    /// Shows the count of unfinished publishes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishSink")
            .field("in_flight", &self.in_flight.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// A client's publish waiting for its writer: where the answer goes.
///
/// If it is dropped without an answer, because the writer stopped with the
/// publish still queued, the client is told storage is unavailable, so the
/// connection's count of unfinished publishes always comes back down.
#[derive(Debug)]
pub(crate) struct ClientReply {
    sink: Arc<PublishSink>,
    id: Option<RequestId>,
    qos: u8,
    bytes: usize,
    answered: bool,
}

impl ClientReply {
    /// Counts the publish as unfinished on its connection.
    ///
    /// Args:
    ///     sink: The connection's sink.
    ///     id: The publish request's id.
    ///     qos: The record's QoS.
    ///     bytes: The record's payload size.
    ///
    /// Returns:
    ///     The reply, to travel with the record to the writer.
    pub(crate) fn new(
        sink: &Arc<PublishSink>,
        id: Option<RequestId>,
        qos: u8,
        bytes: usize,
    ) -> Self {
        sink.begin(bytes);
        Self {
            sink: Arc::clone(sink),
            id,
            qos,
            bytes,
            answered: false,
        }
    }
}

impl Drop for ClientReply {
    /// Answers with `storage_unavailable` if nothing else answered.
    fn drop(&mut self) {
        if !self.answered {
            self.sink.finish(
                "",
                self.id.take(),
                self.qos,
                self.bytes,
                Err(StorageError::Unavailable(
                    "the topic's writer stopped".to_string(),
                )),
            );
        }
    }
}

/// Where a writer sends the result of one publish.
#[derive(Debug)]
pub(crate) enum PublishReply {
    /// A caller inside the server is awaiting the result.
    Waiter(oneshot::Sender<StorageResult<Published>>),
    /// A client connection published it; the writer answers the client.
    Client(ClientReply),
}

impl PublishReply {
    /// Delivers the result of the publish.
    ///
    /// Args:
    ///     topic: The topic the record was published to.
    ///     outcome: What the writer did with the record.
    pub(crate) fn send(self, topic: &str, outcome: StorageResult<Published>) {
        match self {
            PublishReply::Waiter(waiter) => {
                let _ = waiter.send(outcome);
            }
            PublishReply::Client(mut reply) => {
                reply.answered = true;
                reply
                    .sink
                    .finish(topic, reply.id.take(), reply.qos, reply.bytes, outcome);
            }
        }
    }
}
