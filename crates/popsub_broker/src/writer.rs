//! The topic writer: one task per topic that appends to its log, fans new
//! records out, and catches subscriptions up from the log.
//!
//! Every publish, subscribe and catch-up for a topic goes through its
//! writer's queue and is handled one at a time. That gives three properties
//! without any lock held across storage I/O:
//!
//! - Subscribers see a topic's records in `seq` order, because only the
//!   writer sends a topic's records and it sends them in log order.
//! - A subscription's replay and its live records never overlap or leave a
//!   gap: a subscription is only sent the exact `seq` it expects next, and
//!   whatever it misses while it is behind is read back from the log.
//! - The broker lock is only taken for short, non-blocking work, never
//!   while waiting on storage.
//!
//! Replay on subscribe and catch-up after an ACK are the same operation:
//! read the log from the subscription's `next_seq` and send while its window
//! has room. Records are never queued in memory for a subscriber that is
//! behind, so a slow subscriber costs nothing until it catches up, and
//! retention still applies to it (it is told with a `truncated` frame).
//!
//! The cost is that a long catch-up delays publishes to the same topic
//! until it finishes. Other topics are unaffected.

use std::sync::Arc;

use popsub_storage::{NewRecord, Result as StorageResult, Storage};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use popsub_protocol::{RequestId, ServerFrame, ServerMessage, StartFrom, Subscribed, Truncated};

use crate::replay;
use crate::shared::{BrokerCell, LogSettings};
use crate::topic::CursorUpdate;

/// How many records a catch-up reads from the log at a time.
pub(crate) const REPLAY_PAGE: usize = 256;

/// The outcome of a publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The record's position in the topic log.
    pub seq: u64,
    /// The record's message id, generated if the publisher gave none.
    pub message_id: String,
    /// True when the publisher had already published this message id to
    /// this topic and it is still retained. Nothing was stored or delivered.
    pub duplicate: bool,
}

/// The outcome of a subscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscribeOutcome {
    /// The first `seq` the subscription will be sent.
    pub replay_from: u64,
    /// The newest `seq` in the log when it subscribed.
    pub replay_to: u64,
    /// How many records were sent straight away. Fewer than the replay
    /// range when the window filled; the rest follow as acks arrive.
    pub replayed: u64,
    /// Whether a `truncated` frame was sent.
    pub truncated: bool,
    /// False when the client disconnected before the subscription was added.
    pub added: bool,
}

/// Work queued for a topic writer.
#[derive(Debug)]
pub(crate) enum Command {
    /// Append a record and deliver it if it is new.
    Publish {
        /// The record, with its id and time already filled in.
        record: NewRecord,
        /// Where the outcome is sent.
        reply: oneshot::Sender<StorageResult<Published>>,
    },
    /// Add a subscription and replay history to it.
    Subscribe {
        /// The subscribing client.
        client_id: String,
        /// Where replay starts. `Resume` has already been resolved.
        from: StartFrom,
        /// The subscribe request's id, echoed in the `subscribed` frame.
        request_id: Option<RequestId>,
        /// Where the outcome is sent.
        reply: oneshot::Sender<StorageResult<SubscribeOutcome>>,
    },
    /// Send a subscription what it has missed, as far as its window allows.
    CatchUp {
        /// The subscribed client.
        client_id: String,
    },
}

/// The state one topic's writer task owns.
pub(crate) struct TopicWriter {
    /// The topic this writer serves.
    topic: String,
    /// The broker, for fan-out and subscription changes.
    broker: BrokerCell,
    /// Where the topic's log and the session cursors live.
    storage: Arc<dyn Storage>,
    /// Durability for appends and the retention applied after each one.
    log: LogSettings,
    /// The log's `last_seq` as this writer last saw it, or `None` until it
    /// has been read or after a failed append, when it is read again.
    last_seq: Option<u64>,
}

impl TopicWriter {
    /// Starts a writer task for a topic.
    ///
    /// The task runs until every sender for its queue is dropped.
    ///
    /// Args:
    ///     topic: The topic to serve.
    ///     broker: The broker to deliver through.
    ///     storage: Where the topic's log lives.
    ///     log: Durability and retention settings.
    ///
    /// Returns:
    ///     The sending side of the writer's queue.
    pub(crate) fn spawn(
        topic: &str,
        broker: BrokerCell,
        storage: Arc<dyn Storage>,
        log: LogSettings,
    ) -> mpsc::UnboundedSender<Command> {
        let (tx, rx) = mpsc::unbounded_channel();
        let writer = TopicWriter {
            topic: topic.to_string(),
            broker,
            storage,
            log,
            last_seq: None,
        };
        tokio::spawn(writer.run(rx));
        tx
    }

    /// Handles queued commands one at a time until the queue closes.
    ///
    /// A caller that stopped waiting for its result is ignored, and a failed
    /// catch-up is logged; the next ACK or subscribe tries again.
    ///
    /// Args:
    ///     rx: The receiving side of the writer's queue.
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        while let Some(command) = rx.recv().await {
            match command {
                Command::Publish { record, reply } => {
                    let _ = reply.send(self.publish(record).await);
                }
                Command::Subscribe {
                    client_id,
                    from,
                    request_id,
                    reply,
                } => {
                    let _ = reply.send(self.subscribe(&client_id, from, request_id).await);
                }
                Command::CatchUp { client_id } => {
                    if let Err(e) = self.catch_up(&client_id).await {
                        warn!(topic = %self.topic, client = %client_id, %e, "catch-up failed");
                    }
                }
            }
        }
        debug!(topic = %self.topic, "topic writer stopped");
    }

    /// Returns the log's `last_seq`, reading it from storage if not known.
    ///
    /// Returns:
    ///     The highest `seq` the topic has ever had, 0 for a new topic.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the bounds can't be read.
    async fn last_seq(&mut self) -> StorageResult<u64> {
        if let Some(seq) = self.last_seq {
            return Ok(seq);
        }
        let seq = self.storage.bounds(&self.topic).await?.last_seq;
        self.last_seq = Some(seq);
        Ok(seq)
    }

    /// Writes moved cursors to the subscription store.
    ///
    /// A failed write is logged. The cursor in memory has already moved, so
    /// the worst case is that a reconnecting session is sent records it had
    /// already acknowledged, which at-least-once allows.
    ///
    /// Args:
    ///     updates: The cursors to write.
    async fn store_cursors(&self, updates: impl IntoIterator<Item = CursorUpdate>) {
        for update in updates {
            if let Err(e) = self
                .storage
                .ack(&update.session, &update.topic, update.acked_seq)
                .await
            {
                warn!(topic = %update.topic, %e, "storing a cursor failed");
            }
        }
    }

    /// Appends a record, delivers it if it is new, and trims the log.
    ///
    /// A duplicate (same publisher and message id, still retained) is
    /// neither stored again nor delivered again. A failed trim is logged and
    /// doesn't fail the publish; the retention loop tries again later.
    ///
    /// Args:
    ///     record: The record to publish.
    ///
    /// Returns:
    ///     The record's `seq` and id, and whether it was a duplicate.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be read or appended to.
    ///     StorageError::Unsupported: When the backend can't honour the
    ///         configured durability. `SharedBroker::new` checks this, so it
    ///         only happens with a backend that misreports its capabilities.
    async fn publish(&mut self, record: NewRecord) -> StorageResult<Published> {
        let last_seq = self.last_seq().await?;
        let stored = match self
            .storage
            .append(&self.topic, record, self.log.durability)
            .await
        {
            Ok(stored) => stored,
            Err(e) => {
                self.last_seq = None;
                return Err(e);
            }
        };
        let published = Published {
            seq: stored.seq,
            message_id: stored.message_id.clone(),
            duplicate: stored.seq <= last_seq,
        };
        if published.duplicate {
            return Ok(published);
        }
        self.last_seq = Some(stored.seq);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updates = self.broker.with(|broker| broker.deliver(&stored, now_ms));
        self.store_cursors(updates).await;
        if let Err(e) = self
            .storage
            .trim(&self.topic, &self.log.retention, now_ms)
            .await
        {
            warn!(topic = %self.topic, %e, "trimming after publish failed");
        }
        Ok(published)
    }

    /// Adds a subscription, tells the client what will be replayed, and
    /// starts sending it.
    ///
    /// The client gets a `subscribed` frame first, then a `truncated` frame
    /// if part of the requested history was trimmed, then records from the
    /// start of the replay while its window has room. A durable session's
    /// subscription is stored with its cursor just below the replay start.
    ///
    /// Args:
    ///     client_id: The subscribing client.
    ///     from: Where replay starts, with `Resume` already resolved.
    ///     request_id: The subscribe request's id, echoed in `subscribed`.
    ///
    /// Returns:
    ///     The replay range, how much was sent straight away, and whether a
    ///     truncation was reported and the subscription added.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log or the subscription store
    ///         can't be reached. The client is not subscribed and may try again.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    async fn subscribe(
        &mut self,
        client_id: &str,
        from: StartFrom,
        request_id: Option<RequestId>,
    ) -> StorageResult<SubscribeOutcome> {
        let bounds = self.storage.bounds(&self.topic).await?;
        self.last_seq = Some(bounds.last_seq);
        let seq_at = match from {
            StartFrom::SinceMs(time_ms) => self.storage.seq_at(&self.topic, time_ms).await?,
            _ => None,
        };
        let plan = replay::plan(from, bounds, seq_at);
        let mut outcome = SubscribeOutcome {
            replay_from: plan.start,
            replay_to: bounds.last_seq,
            replayed: 0,
            truncated: plan.truncated,
            added: false,
        };
        let Some(session) = self
            .broker
            .with(|broker| broker.clients.get(client_id).map(|c| c.session.clone()))
        else {
            return Ok(outcome);
        };
        if let Some(session) = &session {
            self.storage
                .subscribe(session, &self.topic, plan.start - 1)
                .await?;
        }
        let topic = self.topic.clone();
        let frames = [
            Some(ServerFrame::reply(
                request_id,
                ServerMessage::Subscribed(Subscribed {
                    topic: topic.clone(),
                    replay_from: plan.start,
                    replay_to: bounds.last_seq,
                }),
            )),
            plan.truncated.then(|| {
                ServerFrame::event(ServerMessage::Truncated(Truncated {
                    topic: topic.clone(),
                    first_available: bounds.first_seq,
                }))
            }),
        ];
        outcome.added = self.broker.with(|broker| {
            broker.add_subscriber(&topic, client_id.to_string(), plan.start, bounds.last_seq)
                && frames
                    .iter()
                    .flatten()
                    .all(|frame| broker.send_frame(client_id, frame))
        });
        if outcome.added {
            outcome.replayed = self.catch_up(client_id).await?;
        }
        Ok(outcome)
    }

    /// Sends a subscription the records it has missed, while its window has room.
    ///
    /// Reads the log from the subscription's `next_seq` in pages of
    /// `REPLAY_PAGE`. If records it was due were trimmed, it is sent a
    /// `truncated` frame and moved past them. Stops when the subscription is
    /// up to date, stalled, out of room, or gone.
    ///
    /// Args:
    ///     client_id: The subscribed client.
    ///
    /// Returns:
    ///     How many records were sent.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be read.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    async fn catch_up(&mut self, client_id: &str) -> StorageResult<u64> {
        let last_seq = self.last_seq().await?;
        let mut sent_total = 0;
        while let Some(next) = self
            .broker
            .with(|broker| broker.wants(&self.topic, client_id))
        {
            if next > last_seq {
                break;
            }
            let page = self.storage.read(&self.topic, next, REPLAY_PAGE).await?;
            let first_available = page.first().map_or(last_seq + 1, |r| r.seq);
            if first_available > next {
                let update = self.broker.with(|broker| {
                    broker.send_frame(
                        client_id,
                        &ServerFrame::event(ServerMessage::Truncated(Truncated {
                            topic: self.topic.clone(),
                            first_available,
                        })),
                    );
                    broker.skip_to(&self.topic, client_id, first_available)
                });
                self.store_cursors(update).await;
            }
            if page.is_empty() {
                break;
            }
            let now_ms = chrono::Utc::now().timestamp_millis();
            let (sent, update) = self
                .broker
                .with(|broker| broker.send_records(&self.topic, client_id, &page, now_ms));
            self.store_cursors(update).await;
            sent_total += sent as u64;
            if sent < page.len() {
                break;
            }
        }
        Ok(sent_total)
    }
}
