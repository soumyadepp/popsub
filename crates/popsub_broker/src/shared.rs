//! `SharedBroker`, the one handle every task uses to reach the broker and
//! the topic logs.
//!
//! The broker itself is a plain synchronous struct behind a mutex. Only
//! `with` (and, inside this crate, `BrokerCell::with`) locks it, by running a
//! closure under the lock, so no caller can hold it across an await point
//! and the poisoning policy lives in one place.
//!
//! Publishing and subscribing touch storage, so they are `async` and go
//! through the topic's writer task (see `writer`), which is started the
//! first time a topic is used. The writers hold the broker and the storage
//! but not this handle, so they stop once every `SharedBroker` clone is
//! dropped.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use popsub_storage::{
    Durability, NewRecord, Result as StorageResult, Retention, Storage, StorageError,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::engine::Broker;
use crate::frame::StartFrom;
use crate::writer::{Command, Published, Subscribed, TopicWriter};

/// How often `run_retry_loop` looks for overdue QoS 1 records.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// How often `run_retention_loop` trims every topic, so topics nobody
/// publishes to still lose records past their age limit.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(60);

/// How firmly appends are stored and how much history each topic keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSettings {
    /// The durability every append is stored at.
    pub durability: Durability,
    /// The limits applied after each append and by the retention loop.
    pub retention: Retention,
}

/// The broker behind its mutex, the only way any code locks it.
#[derive(Clone, Debug)]
pub(crate) struct BrokerCell(
    /// The broker and the lock that guards it.
    Arc<Mutex<Broker>>,
);

impl BrokerCell {
    /// Runs `f` with exclusive access to the broker and returns its result.
    ///
    /// If a previous holder panicked, the lock is poisoned. Rather than taking
    /// the whole server down with it, the error is logged and the broker is
    /// used as it was left: every broker operation leaves its maps usable,
    /// even when interrupted.
    ///
    /// Args:
    ///     f: The work to do while holding the lock. It must not block.
    ///
    /// Returns:
    ///     Whatever `f` returns.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Broker) -> R) -> R {
        let mut guard = self.lock();
        f(&mut guard)
    }

    /// Takes the lock, recovering from poisoning as `with` describes.
    ///
    /// Returns:
    ///     A guard giving exclusive access to the broker.
    fn lock(&self) -> MutexGuard<'_, Broker> {
        self.0.lock().unwrap_or_else(|poisoned| {
            error!("broker lock was poisoned by a panicking task, continuing with its state");
            poisoned.into_inner()
        })
    }
}

/// The queues of the topic writers started so far, by topic.
type Writers = HashMap<String, mpsc::UnboundedSender<Command>>;

/// A cloneable handle to the broker, the storage and the topic writers.
///
/// Cloning is cheap and every clone refers to the same broker.
#[derive(Clone)]
pub struct SharedBroker {
    /// The broker behind the only lock that guards it.
    broker: BrokerCell,
    /// Where topic logs live.
    storage: Arc<dyn Storage>,
    /// Durability and retention for every topic.
    log: LogSettings,
    /// One writer queue per topic used so far.
    writers: Arc<Mutex<Writers>>,
}

impl fmt::Debug for SharedBroker {
    /// Shows the log settings and the storage's capabilities; the broker's
    /// own state is left out because printing it would need the lock.
    ///
    /// Args:
    ///     f: The formatter.
    ///
    /// Raises:
    ///     fmt::Error: When the formatter fails.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedBroker")
            .field("log", &self.log)
            .field("capabilities", &self.storage.capabilities())
            .finish_non_exhaustive()
    }
}

impl SharedBroker {
    /// Wraps a broker and a storage backend so they can be shared between tasks.
    ///
    /// Args:
    ///     broker: The broker to share.
    ///     storage: Where topic logs live.
    ///     log: Durability and retention for every topic.
    ///
    /// Returns:
    ///     A handle to the broker.
    ///
    /// Raises:
    ///     StorageError::Unsupported: When the backend can't provide the
    ///         requested durability. This is refused rather than downgraded.
    pub fn new(broker: Broker, storage: Arc<dyn Storage>, log: LogSettings) -> StorageResult<Self> {
        storage.capabilities().check(log.durability)?;
        Ok(Self {
            broker: BrokerCell(Arc::new(Mutex::new(broker))),
            storage,
            log,
            writers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Runs `f` with exclusive access to the broker and returns its result.
    ///
    /// See `BrokerCell::with` for how a poisoned lock is handled.
    ///
    /// Args:
    ///     f: The work to do while holding the lock. It must not block.
    ///
    /// Returns:
    ///     Whatever `f` returns.
    pub fn with<R>(&self, f: impl FnOnce(&mut Broker) -> R) -> R {
        self.broker.with(f)
    }

    /// Publishes a record to a topic and delivers it to current subscribers.
    ///
    /// The publish time is set to now and an empty message id is replaced
    /// with a new UUID. The call returns once the record is stored at the
    /// configured durability and queued for every subscriber.
    ///
    /// Args:
    ///     topic: The topic to publish to.
    ///     record: The record. `publisher`, `payload` and `qos` are kept.
    ///
    /// Returns:
    ///     The record's `seq` and whether it was a duplicate.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When storage fails or the topic's
    ///         writer stopped.
    ///     StorageError::Unsupported: When the backend refuses the durability.
    pub async fn publish(&self, topic: &str, mut record: NewRecord) -> StorageResult<Published> {
        record.published_at_ms = chrono::Utc::now().timestamp_millis();
        if record.message_id.is_empty() {
            record.message_id = Uuid::new_v4().to_string();
        }
        self.request(topic, |reply| Command::Publish {
            record: record.clone(),
            reply,
        })
        .await
    }

    /// Replays a topic's history to a client as asked, then subscribes it.
    ///
    /// Args:
    ///     topic: The topic to subscribe to.
    ///     client_id: The registered client subscribing.
    ///     from: Where replay starts.
    ///
    /// Returns:
    ///     What was replayed and whether the subscription was added.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be read or the
    ///         topic's writer stopped. The client is not subscribed.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    pub async fn subscribe(
        &self,
        topic: &str,
        client_id: &str,
        from: StartFrom,
    ) -> StorageResult<Subscribed> {
        self.request(topic, |reply| Command::Subscribe {
            client_id: client_id.to_string(),
            from,
            reply,
        })
        .await
    }

    /// Sends a command to a topic's writer and waits for its result.
    ///
    /// If the writer's queue is closed (its task panicked), a new writer is
    /// started and the command is sent once more.
    ///
    /// Args:
    ///     topic: The topic whose writer handles the command.
    ///     build: Builds the command around the channel its result comes back on.
    ///
    /// Returns:
    ///     The writer's result.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When no writer accepts the command or
    ///         the writer stops before answering, plus anything the command
    ///         itself returns.
    async fn request<T>(
        &self,
        topic: &str,
        build: impl Fn(oneshot::Sender<StorageResult<T>>) -> Command,
    ) -> StorageResult<T> {
        for _ in 0..2 {
            let (reply, result) = oneshot::channel();
            if self.writer(topic).send(build(reply)).is_ok() {
                return result.await.unwrap_or_else(|_| {
                    Err(StorageError::Unavailable(format!(
                        "the writer for topic '{topic}' stopped"
                    )))
                });
            }
            warn!(topic, "topic writer had stopped, starting a new one");
            self.writers().remove(topic);
        }
        Err(StorageError::Unavailable(format!(
            "no writer for topic '{topic}'"
        )))
    }

    /// Returns a topic's writer queue, starting the writer if needed.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The sending side of the writer's queue.
    fn writer(&self, topic: &str) -> mpsc::UnboundedSender<Command> {
        self.writers()
            .entry(topic.to_string())
            .or_insert_with(|| {
                TopicWriter::spawn(topic, self.broker.clone(), self.storage.clone(), self.log)
            })
            .clone()
    }

    /// Locks the writer map, recovering it if a holder panicked. Nothing
    /// panics while holding it, so its contents are always usable.
    ///
    /// Returns:
    ///     The guard over the writer map.
    fn writers(&self) -> MutexGuard<'_, Writers> {
        self.writers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Resends overdue QoS 1 records every `RETRY_INTERVAL`, forever.
    ///
    /// Spawn this as a background task. Each tick runs one
    /// `Broker::retry_overdue` pass under the lock.
    pub async fn run_retry_loop(self) {
        let mut ticker = tokio::time::interval(RETRY_INTERVAL);
        loop {
            ticker.tick().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let outcome = self.with(|broker| broker.retry_overdue(now_ms));
            if outcome.resent > 0 || outcome.dropped > 0 {
                info!(
                    resent = outcome.resent,
                    dropped = outcome.dropped,
                    "retried QoS 1 messages"
                );
            }
        }
    }

    /// Trims every topic log to the retention limits every `RETENTION_INTERVAL`, forever.
    ///
    /// Publishing already trims the topic it appends to; this catches topics
    /// that have gone quiet. Spawn it as a background task. Failures are
    /// logged and retried on the next tick.
    pub async fn run_retention_loop(self) {
        let mut ticker = tokio::time::interval(RETENTION_INTERVAL);
        loop {
            ticker.tick().await;
            self.trim_all(chrono::Utc::now().timestamp_millis()).await;
        }
    }

    /// Trims every topic log once.
    ///
    /// Args:
    ///     now_ms: The current time, for the age limit.
    ///
    /// Returns:
    ///     How many records were removed across all topics.
    pub async fn trim_all(&self, now_ms: i64) -> u64 {
        let topics = match self.storage.topics().await {
            Ok(topics) => topics,
            Err(e) => {
                warn!(%e, "listing topics for retention failed");
                return 0;
            }
        };
        let mut removed = 0;
        for topic in topics {
            match self.storage.trim(&topic, &self.log.retention, now_ms).await {
                Ok(n) => removed += n,
                Err(e) => warn!(topic, %e, "trimming failed"),
            }
        }
        if removed > 0 {
            info!(removed, "trimmed topic logs");
        }
        removed
    }
}
