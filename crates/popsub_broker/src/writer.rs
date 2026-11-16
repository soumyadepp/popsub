//! The topic writer: one task per topic that appends to its log, fans new
//! records out, and replays history to new subscribers.
//!
//! Every publish and subscribe for a topic goes through its writer's queue
//! and is handled one at a time. That gives three properties without any
//! lock held across storage I/O:
//!
//! - Subscribers see a topic's records in `seq` order, because only the
//!   writer delivers new records and it delivers them in the order the log
//!   numbered them.
//! - A subscriber's replay and its live records never overlap or leave a
//!   gap, because no publish to the topic is handled between reading the
//!   log's bounds, replaying up to them, and adding the subscriber.
//! - The broker lock is only taken for the short, non-blocking fan-out and
//!   subscriber changes, never while waiting on storage.
//!
//! The cost is that a long replay delays publishes to the same topic until
//! it finishes. Other topics are unaffected.

use std::sync::Arc;

use popsub_storage::{NewRecord, Result as StorageResult, Storage};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::frame::{Delivery, StartFrom, TopicFrame, Truncated};
use crate::replay;
use crate::shared::{BrokerCell, LogSettings};

/// How many records a replay reads from the log at a time.
pub(crate) const REPLAY_PAGE: usize = 256;

/// The outcome of a publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Published {
    /// The record's position in the topic log.
    pub seq: u64,
    /// True when the publisher had already published this message id to
    /// this topic and it is still retained. Nothing was stored or delivered.
    pub duplicate: bool,
}

/// The outcome of a subscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscribed {
    /// How many stored records were replayed before live delivery started.
    pub replayed: u64,
    /// Whether a `truncated` frame was sent because records the subscriber
    /// asked for were already trimmed.
    pub truncated: bool,
    /// False when the client disconnected before the subscription was added.
    pub added: bool,
}

/// Work queued for a topic writer, each with a channel for its result.
#[derive(Debug)]
pub(crate) enum Command {
    /// Append a record and deliver it if it is new.
    Publish {
        /// The record, with its id and time already filled in.
        record: NewRecord,
        /// Where the outcome is sent.
        reply: oneshot::Sender<StorageResult<Published>>,
    },
    /// Replay history to a client, then add it as a subscriber.
    Subscribe {
        /// The subscribing client.
        client_id: String,
        /// Where replay starts.
        from: StartFrom,
        /// Where the outcome is sent.
        reply: oneshot::Sender<StorageResult<Subscribed>>,
    },
}

/// The state one topic's writer task owns.
pub(crate) struct TopicWriter {
    /// The topic this writer serves.
    topic: String,
    /// The broker, for fan-out and subscriber changes.
    broker: BrokerCell,
    /// Where the topic's log lives.
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
    /// A caller that stopped waiting for its result is ignored.
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
                    reply,
                } => {
                    let _ = reply.send(self.subscribe(&client_id, from).await);
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
    ///     The record's `seq` and whether it was a duplicate.
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
        if stored.seq <= last_seq {
            return Ok(Published {
                seq: stored.seq,
                duplicate: true,
            });
        }
        self.last_seq = Some(stored.seq);
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.broker.with(|broker| broker.deliver(&stored, now_ms));
        if let Err(e) = self
            .storage
            .trim(&self.topic, &self.log.retention, now_ms)
            .await
        {
            warn!(topic = %self.topic, %e, "trimming after publish failed");
        }
        Ok(Published {
            seq: stored.seq,
            duplicate: false,
        })
    }

    /// Replays the requested history to a client, then adds it as a subscriber.
    ///
    /// A `truncated` frame goes first when part of the requested history was
    /// trimmed, and another one mid-replay if the retention loop trims
    /// records before they are read. Records are read in pages of
    /// `REPLAY_PAGE` and sent with their original `seq`. If the client disconnects during replay, the
    /// replay stops and no subscription is added.
    ///
    /// Args:
    ///     client_id: The subscribing client.
    ///     from: Where replay starts.
    ///
    /// Returns:
    ///     How many records were replayed, whether a truncation was reported,
    ///     and whether the subscription was added.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be read. The client
    ///         is not subscribed and may try again.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    async fn subscribe(&mut self, client_id: &str, from: StartFrom) -> StorageResult<Subscribed> {
        let bounds = self.storage.bounds(&self.topic).await?;
        self.last_seq = Some(bounds.last_seq);
        let seq_at = match from {
            StartFrom::SinceMs(time_ms) => self.storage.seq_at(&self.topic, time_ms).await?,
            _ => None,
        };
        let plan = replay::plan(from, bounds, seq_at);
        let not_added = Subscribed {
            replayed: 0,
            truncated: false,
            added: false,
        };
        let Some(sender) = self.broker.with(|broker| broker.client_sender(client_id)) else {
            return Ok(not_added);
        };
        let mut truncated = plan.truncated;
        if plan.truncated && !self.send_truncated(&sender, bounds.first_seq) {
            return Ok(not_added);
        }
        let mut next = plan.start;
        let mut replayed = 0;
        while next <= bounds.last_seq {
            let page = self.storage.read(&self.topic, next, REPLAY_PAGE).await?;
            let (Some(first), Some(last)) = (page.first(), page.last()) else {
                break;
            };
            if first.seq > next {
                truncated = true;
                if !self.send_truncated(&sender, first.seq) {
                    return Ok(not_added);
                }
            }
            next = last.seq + 1;
            for record in page.iter().filter(|r| r.seq <= bounds.last_seq) {
                if let Some(ws) = TopicFrame::Message(Delivery::from(record)).to_ws() {
                    if sender.send(ws).is_err() {
                        return Ok(not_added);
                    }
                    replayed += 1;
                }
            }
        }
        let added = self
            .broker
            .with(|broker| broker.add_subscriber(&self.topic, client_id.to_string()));
        Ok(Subscribed {
            replayed,
            truncated,
            added,
        })
    }

    /// Sends a `truncated` frame for this topic to one client.
    ///
    /// Args:
    ///     sender: The client's outgoing channel.
    ///     first_available: The oldest `seq` the client will receive.
    ///
    /// Returns:
    ///     False when the client's channel is closed.
    fn send_truncated(
        &self,
        sender: &mpsc::UnboundedSender<tungstenite::protocol::Message>,
        first_available: u64,
    ) -> bool {
        let frame = TopicFrame::Truncated(Truncated {
            topic: self.topic.clone(),
            first_available,
        });
        frame.to_ws().is_none_or(|ws| sender.send(ws).is_ok())
    }
}
