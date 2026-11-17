//! `SharedBroker`, the one handle every task uses to reach the broker and
//! the topic logs.
//!
//! The broker itself is a plain synchronous struct behind a mutex. Only
//! `with` (and, inside this crate, `BrokerCell::with`) locks it, by running a
//! closure under the lock, so no caller can hold it across an await point
//! and the poisoning policy lives in one place.
//!
//! Publishing, subscribing and anything that moves a stored cursor touch
//! storage, so they are `async`. Publishes, subscribes and catch-ups go
//! through the topic's writer task (see `writer`), which is started the
//! first time a topic is used. The writers hold the broker and the storage
//! but not this handle, so they stop once every `SharedBroker` clone is
//! dropped.
//!
//! Durable sessions are keyed in the subscription store by `session_key`,
//! which combines the username with the client's session id, so one user
//! can never attach to another user's session.

use crate::engine::{BrokerStats, RetryOutcome};
use crate::health::StorageHealth;
use popsub_protocol::TopicInfo;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use popsub_storage::{
    Durability, NewRecord, Result as StorageResult, Retention, Storage, StorageError, Subscription,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::engine::Broker;
use crate::writer::{Command, Published, SubscribeOutcome, TopicWriter};
use popsub_protocol::{CloseCode, RequestId, StartFrom};

/// How often `run_retry_loop` looks for overdue QoS 1 records.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// How often `run_retention_loop` trims every topic, so topics nobody
/// publishes to still lose records past their age limit.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(60);

/// How often `run_session_expiry_loop` touches connected sessions and
/// removes expired ones.
pub const SESSION_EXPIRY_INTERVAL: Duration = Duration::from_secs(60);

/// Builds the subscription store key for a user's durable session.
///
/// The username's length goes first, so no username and session id pair
/// can produce the same key as another, whatever characters they contain.
///
/// Args:
///     username: The authenticated user.
///     session_id: The id the client chose.
///
/// Returns:
///     The key.
pub fn session_key(username: &str, session_id: &str) -> String {
    format!("{}:{username}:{session_id}", username.len())
}

/// The current time in milliseconds since the Unix epoch.
///
/// Returns:
///     The time.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// How firmly appends are stored, how much history each topic keeps, and
/// how long durable sessions last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSettings {
    /// The durability every append is stored at.
    pub durability: Durability,
    /// The limits applied after each append and by the retention loop.
    pub retention: Retention,
    /// How long a durable session is kept after its last connection.
    pub session_expiry: Duration,
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
    /// The outcomes of recent appends, for readiness.
    health: Arc<StorageHealth>,
    /// Retention for particular topics: the first pattern a topic matches
    /// applies; topics matching none use `log.retention`.
    topic_retention: Arc<Vec<(String, Retention)>>,
    /// Decides whether a user may read a topic, when a new topic is about
    /// to be joined through a wildcard subscription. Unset allows everyone.
    authorizer: Arc<std::sync::OnceLock<Authorizer>>,
}

/// Decides whether a user (first argument) may subscribe to a topic (second).
pub type Authorizer = Arc<
    dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

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
            health: Arc::new(StorageHealth::default()),
            topic_retention: Arc::new(Vec::new()),
            authorizer: Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Sets who may read which topic, asked before a wildcard subscription
    /// joins a newly created topic. Only the first call takes effect.
    ///
    /// Args:
    ///     authorizer: The check.
    pub fn set_authorizer(&self, authorizer: Authorizer) {
        let _ = self.authorizer.set(authorizer);
    }

    /// The names of every topic that has a log.
    ///
    /// Returns:
    ///     The topic names, sorted.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the logs can't be listed.
    pub async fn topic_names(&self) -> StorageResult<Vec<String>> {
        self.storage.topics().await
    }

    /// Subscribes a client to a topic because one of its wildcard
    /// subscriptions matches it. The client gets a `subscribed` frame with
    /// no id for the topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///     client_id: The client.
    ///     from: Where replay starts.
    ///
    /// Returns:
    ///     What was replayed and whether the subscription was added.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log or the subscription store
    ///         can't be reached.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    pub async fn join(
        &self,
        topic: &str,
        client_id: &str,
        from: StartFrom,
    ) -> StorageResult<SubscribeOutcome> {
        let outcome = self.subscribe_inner(topic, client_id, from, None).await?;
        self.with(|broker| broker.mark_joined(&client_id.to_string(), topic));
        Ok(outcome)
    }

    /// Joins a topic that just got its first record for every client with a
    /// matching wildcard subscription that may read it, from that first record.
    ///
    /// Args:
    ///     topic: The new topic.
    async fn join_pattern_holders(&self, topic: &str) {
        let holders = self.with(|broker| broker.pattern_holders(topic));
        for (client_id, username) in holders {
            let allowed = match self.authorizer.get() {
                Some(authorizer) => authorizer(username, topic.to_string()).await,
                None => true,
            };
            if !allowed {
                continue;
            }
            if let Err(e) = self.join(topic, &client_id, StartFrom::Seq(1)).await {
                warn!(topic, client = %client_id, %e, "joining a new topic for a wildcard subscription failed");
            }
        }
    }

    /// Sets retention for particular topics. Call it before the broker is
    /// used: a topic's writer keeps the policy it started with.
    ///
    /// Args:
    ///     rules: Topic patterns with their policies; the first match applies.
    ///
    /// Returns:
    ///     The broker with the rules.
    pub fn with_topic_retention(mut self, rules: Vec<(String, Retention)>) -> Self {
        self.topic_retention = Arc::new(rules);
        self
    }

    /// The retention policy for a topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The policy of the first matching rule, or the default policy.
    pub fn retention_for(&self, topic: &str) -> Retention {
        self.topic_retention
            .iter()
            .find(|(pattern, _)| popsub_protocol::topics::matches_pattern(pattern, topic))
            .map_or(self.log.retention, |(_, retention)| *retention)
    }

    /// The last `seq` stored on a topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The last `seq`, 0 for a topic with no records yet.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be read.
    pub async fn last_seq(&self, topic: &str) -> StorageResult<u64> {
        self.storage
            .bounds(topic)
            .await
            .map(|bounds| bounds.last_seq)
    }

    /// Lists every topic that has a log, with its bounds and live subscribers.
    ///
    /// Returns:
    ///     The topics, sorted by name.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the logs can't be read.
    pub async fn topic_infos(&self) -> StorageResult<Vec<TopicInfo>> {
        let mut infos = Vec::new();
        for name in self.storage.topics().await? {
            if let Some(info) = self.topic_info(&name).await? {
                infos.push(info);
            }
        }
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(infos)
    }

    /// Describes one topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     Its bounds, record count and live subscribers, or `None` when
    ///     nothing was ever published to it.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be read.
    pub async fn topic_info(&self, topic: &str) -> StorageResult<Option<TopicInfo>> {
        let bounds = self.storage.bounds(topic).await?;
        if bounds.last_seq == 0 {
            return Ok(None);
        }
        let subscribers = self.with(|broker| {
            broker
                .topics
                .get(topic)
                .map_or(0, |t| t.subscribers.len() as u64)
        });
        Ok(Some(TopicInfo {
            name: topic.to_string(),
            first_seq: bounds.first_seq,
            last_seq: bounds.last_seq,
            messages: (bounds.last_seq + 1).saturating_sub(bounds.first_seq),
            subscribers,
        }))
    }

    /// Removes every record a topic holds. Its numbering carries on, and
    /// subscribers that hadn't caught up are told with `truncated` when they
    /// next read.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Returns:
    ///     The topic afterwards, or `None` when nothing was ever published to it.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log can't be changed.
    pub async fn purge_topic(&self, topic: &str) -> StorageResult<Option<TopicInfo>> {
        if self.topic_info(topic).await?.is_none() {
            return Ok(None);
        }
        let nothing = Retention {
            max_messages: Some(0),
            max_age_ms: None,
            max_bytes: None,
        };
        let removed = self.storage.trim(topic, &nothing, now_ms()).await?;
        info!(topic, removed, "purged a topic");
        self.topic_info(topic).await
    }

    /// A snapshot of the broker's size, for gauges.
    ///
    /// Returns:
    ///     Connections, topics, subscriptions and records in flight.
    pub fn stats(&self) -> BrokerStats {
        self.with(|broker| broker.stats())
    }

    /// Whether appends are failing, for the server's readiness check.
    ///
    /// Args:
    ///     window: How long a failed append counts for, unless one succeeds after it.
    ///
    /// Returns:
    ///     How long ago the last failure was, or `None` when storage isn't failing.
    pub fn storage_failing(&self, window: Duration) -> Option<Duration> {
        self.health.failing(window)
    }

    /// The window size subscriptions get, for the limits clients are told about.
    ///
    /// Returns:
    ///     The most unacknowledged QoS 1 records per subscription.
    pub fn max_inflight(&self) -> usize {
        self.with(|broker| broker.max_inflight())
    }

    /// The connection limit from the broker settings, which the transport enforces.
    ///
    /// Returns:
    ///     The most connections at once, or `None` for no limit.
    pub fn max_connections(&self) -> Option<usize> {
        self.with(|broker| broker.max_connections())
    }

    /// How long a durable session outlives its last connection, for the
    /// limits clients are told about.
    ///
    /// Returns:
    ///     The session expiry.
    pub fn session_expiry(&self) -> Duration {
        self.log.session_expiry
    }

    /// Closes every connection authenticated as a user, for example when an
    /// admin disables or deletes the account.
    ///
    /// Args:
    ///     username: The user.
    ///     code: Why, usually `CloseCode::AccountDisabled`.
    ///
    /// Returns:
    ///     How many open connections were closed.
    pub fn close_user(&self, username: &str, code: CloseCode) -> usize {
        self.with(|broker| {
            broker
                .clients
                .values()
                .filter(|client| client.username.as_deref() == Some(username))
                .filter(|client| {
                    let open = client.sender.closing().is_none();
                    client.sender.close(code);
                    open
                })
                .count()
        })
    }

    /// Closes every connected client's outgoing queue, for a graceful shutdown.
    ///
    /// Each send loop writes what is already queued, then a close frame with
    /// the code; the connections clean up as usual.
    ///
    /// Args:
    ///     code: Why the connections are being closed, usually `GoingAway`.
    ///
    /// Returns:
    ///     How many clients were open and are now closing.
    pub fn close_all(&self, code: CloseCode) -> usize {
        self.with(|broker| {
            broker
                .clients
                .values()
                .filter(|client| {
                    let open = client.sender.closing().is_none();
                    client.sender.close(code);
                    open
                })
                .count()
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
        record.published_at_ms = now_ms();
        if record.message_id.is_empty() {
            record.message_id = Uuid::new_v4().to_string();
        }
        let published = self
            .request(topic, |reply| Command::Publish {
                record: record.clone(),
                reply,
            })
            .await?;
        if published.seq == 1 && !published.duplicate {
            self.join_pattern_holders(topic).await;
        }
        Ok(published)
    }

    /// Subscribes a client to a topic and starts replaying the history it asked for.
    ///
    /// `Resume` becomes `Seq` one past the stored cursor when the client's
    /// durable session has a stored subscription to the topic, and `New`
    /// otherwise.
    ///
    /// Args:
    ///     topic: The topic to subscribe to.
    ///     client_id: The registered client subscribing.
    ///     from: Where replay starts.
    ///     request_id: The subscribe request's id, echoed in `subscribed`.
    ///
    /// Returns:
    ///     The replay range and whether the subscription was added.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log or the subscription store
    ///         can't be reached, or the topic's writer stopped. The client is
    ///         not subscribed.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    pub async fn subscribe(
        &self,
        topic: &str,
        client_id: &str,
        from: StartFrom,
        request_id: Option<RequestId>,
    ) -> StorageResult<SubscribeOutcome> {
        let outcome = self
            .subscribe_inner(topic, client_id, from, request_id)
            .await?;
        self.with(|broker| broker.mark_explicit(&client_id.to_string(), topic));
        Ok(outcome)
    }

    /// Subscribes a client to a topic through the topic's writer, for both
    /// `subscribe` (by name) and `join` (through a wildcard subscription).
    ///
    /// Args:
    ///     topic: The topic to subscribe to.
    ///     client_id: The registered client subscribing.
    ///     from: Where replay starts.
    ///     request_id: The subscribe request's id, echoed in `subscribed`.
    ///
    /// Returns:
    ///     What was replayed and whether the subscription was added.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the log or the subscription store
    ///         can't be reached.
    ///     StorageError::Corrupt: When a stored record fails its checks.
    async fn subscribe_inner(
        &self,
        topic: &str,
        client_id: &str,
        from: StartFrom,
        request_id: Option<RequestId>,
    ) -> StorageResult<SubscribeOutcome> {
        let from = match from {
            StartFrom::Resume => self.resume_point(topic, client_id).await?,
            other => other,
        };
        self.request(topic, |reply| Command::Subscribe {
            client_id: client_id.to_string(),
            from,
            request_id: request_id.clone(),
            reply,
        })
        .await
    }

    /// Works out where `Resume` starts for a client and topic.
    ///
    /// Args:
    ///     topic: The topic.
    ///     client_id: The client.
    ///
    /// Returns:
    ///     `Seq` one past the stored cursor, or `New` when there is none.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the subscription store can't be read.
    async fn resume_point(&self, topic: &str, client_id: &str) -> StorageResult<StartFrom> {
        let Some(session) = self.with(|broker| broker.client_session(client_id)) else {
            return Ok(StartFrom::New);
        };
        let stored = self.storage.load(&session).await?;
        Ok(stored
            .iter()
            .find(|s| s.topic == topic)
            .map_or(StartFrom::New, |s| StartFrom::Seq(s.acked_seq + 1)))
    }

    /// Unsubscribes a client from a topic, and forgets the stored
    /// subscription if the client holds a durable session.
    ///
    /// Args:
    ///     topic: The topic to leave.
    ///     client_id: The client.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the stored subscription can't be removed.
    pub async fn unsubscribe(&self, topic: &str, client_id: &str) -> StorageResult<()> {
        let session = self.with(|broker| broker.unsubscribe(topic, &client_id.to_string()));
        match session {
            Some(session) => self.storage.unsubscribe(&session, topic).await,
            None => Ok(()),
        }
    }

    /// Records a client's acknowledgement of one record on one topic.
    ///
    /// Only a record in flight to this client is accepted. An accepted ACK
    /// stores the session's cursor if it moved, and asks the topic's writer
    /// to send whatever the subscription missed while its window was full.
    ///
    /// Args:
    ///     client_id: The acknowledging client.
    ///     topic: The topic the record was delivered on.
    ///     seq: The acknowledged record.
    ///
    /// Returns:
    ///     Whether the ACK was accepted.
    pub async fn ack(&self, client_id: &str, topic: &str, seq: u64) -> bool {
        let outcome = self.with(|broker| broker.ack(client_id, topic, seq, now_ms()));
        if let Some(update) = &outcome.cursor
            && let Err(e) = self
                .storage
                .ack(&update.session, &update.topic, update.acked_seq)
                .await
        {
            warn!(topic, %e, "storing a cursor failed");
        }
        if outcome.catch_up {
            self.send(
                topic,
                Command::CatchUp {
                    client_id: client_id.to_string(),
                },
            );
        }
        outcome.accepted
    }

    /// Binds a durable session to a connected client.
    ///
    /// Another connection already holding the session is sent a close frame
    /// with `CloseCode::SessionTakenOver` and removed first, so a client that reconnects before the server noticed its old
    /// connection drop takes the session over. The session is touched for
    /// expiry. With `clean_start`, its stored subscriptions are discarded.
    ///
    /// Args:
    ///     client_id: The connected client.
    ///     session: The session's storage key, from `session_key`.
    ///     clean_start: Whether to discard the stored subscriptions.
    ///
    /// Returns:
    ///     The stored subscriptions to restore, empty after a clean start or
    ///     for a new session.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the subscription store can't be reached.
    pub async fn attach_session(
        &self,
        client_id: &str,
        session: &str,
        clean_start: bool,
    ) -> StorageResult<Vec<Subscription>> {
        self.with(|broker| {
            if let Some(old) = broker.session_holder(session)
                && old != client_id
            {
                if let Some(sender) = broker.client_sender(&old) {
                    sender.close(CloseCode::SessionTakenOver);
                }
                broker.cleanup_client(&old);
                info!(old_client = %old, client = %client_id, "session taken over by a new connection");
            }
            if let Some(client) = broker.clients.get_mut(client_id) {
                client.session = Some(session.to_string());
            }
        });
        self.storage.touch(session, now_ms()).await?;
        let stored = self.storage.load(session).await?;
        if !clean_start {
            return Ok(stored);
        }
        for subscription in stored {
            self.storage
                .unsubscribe(session, &subscription.topic)
                .await?;
        }
        Ok(Vec::new())
    }

    /// Removes a stored subscription the session may no longer have, such as
    /// one whose topic permission was withdrawn.
    ///
    /// Args:
    ///     session: The session's storage key.
    ///     topic: The topic.
    ///
    /// Raises:
    ///     StorageError::Unavailable: When the subscription store can't be changed.
    pub async fn forget_subscription(&self, session: &str, topic: &str) -> StorageResult<()> {
        self.storage.unsubscribe(session, topic).await
    }

    /// Removes a disconnected client and all its subscriptions.
    ///
    /// If it held a durable session, the session is touched in the
    /// background, so its expiry counts from the disconnect.
    ///
    /// Args:
    ///     client_id: The client to remove.
    pub fn cleanup_client(&self, client_id: &str) {
        let session = self.with(|broker| broker.cleanup_client(&client_id.to_string()));
        if let Some(session) = session {
            let storage = self.storage.clone();
            tokio::spawn(async move {
                if let Err(e) = storage.touch(&session, now_ms()).await {
                    warn!(%e, "touching a session on disconnect failed");
                }
            });
        }
    }

    /// Queues a command to a topic's writer without waiting for it.
    ///
    /// Args:
    ///     topic: The topic whose writer handles the command.
    ///     command: The command.
    fn send(&self, topic: &str, command: Command) {
        if let Err(mpsc::error::SendError(command)) = self.writer(topic).send(command) {
            self.writers().remove(topic);
            let _ = self.writer(topic).send(command);
        }
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
                TopicWriter::spawn(
                    topic,
                    self.broker.clone(),
                    self.storage.clone(),
                    LogSettings {
                        retention: self.retention_for(topic),
                        ..self.log
                    },
                    self.health.clone(),
                )
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
            let outcome = self.retry_pass(now_ms()).await;
            if outcome.resent > 0 || outcome.stalled > 0 || outcome.dead_lettered > 0 {
                info!(
                    resent = outcome.resent,
                    stalled = outcome.stalled,
                    dead_lettered = outcome.dead_lettered,
                    "retried QoS 1 messages"
                );
            }
        }
    }

    /// Runs one retry pass: resends overdue records, then copies the ones
    /// given up on to the dead-letter topic, stores the cursors that moved
    /// past them and catches the freed subscriptions up.
    ///
    /// A dead letter is a new record on `broker.dead_letter_topic`, published
    /// by `popsub`, whose payload is a JSON object: the original `topic`,
    /// `seq`, `message_id`, `publisher`, `payload` and `published_at_ms`, the
    /// `subscriber` (username) that didn't acknowledge it, and `reason`
    /// (`max_retries`). If that publish fails it is logged; the original
    /// record is still in its own topic's log.
    ///
    /// Args:
    ///     now_ms: The current time in milliseconds since the Unix epoch.
    ///
    /// Returns:
    ///     What the pass did.
    pub async fn retry_pass(&self, now_ms: i64) -> RetryOutcome {
        let (outcome, dead, dead_letter_topic) = self.with(|broker| {
            (
                broker.retry_overdue(now_ms),
                broker.take_dead_letters(),
                broker.dead_letter_topic().to_string(),
            )
        });
        for update in dead.cursors {
            if let Err(e) = self
                .storage
                .ack(&update.session, &update.topic, update.acked_seq)
                .await
            {
                warn!(topic = %update.topic, %e, "storing a cursor failed");
            }
        }
        for letter in dead.letters {
            let envelope = serde_json::json!({
                "topic": letter.record.topic,
                "seq": letter.record.seq,
                "message_id": letter.record.message_id,
                "publisher": letter.record.publisher,
                "payload": letter.record.payload,
                "published_at_ms": letter.record.published_at_ms,
                "subscriber": letter.username,
                "reason": "max_retries",
            });
            let record = NewRecord {
                message_id: uuid::Uuid::new_v4().to_string(),
                publisher: "popsub".to_string(),
                payload: envelope.to_string(),
                qos: 1,
                published_at_ms: now_ms,
            };
            if let Err(e) = self.publish(&dead_letter_topic, record).await {
                error!(topic = %letter.record.topic, seq = letter.record.seq, %e, "copying a record to the dead-letter topic failed");
            }
        }
        for (topic, client_id) in dead.catch_up {
            self.send(&topic, Command::CatchUp { client_id });
        }
        outcome
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
            self.trim_all(now_ms()).await;
        }
    }

    /// Expires durable sessions not seen for `LogSettings::session_expiry`,
    /// every `SESSION_EXPIRY_INTERVAL`, forever.
    ///
    /// Connected sessions are touched first, so a long-lived connection never
    /// loses its session. Spawn it as a background task.
    pub async fn run_session_expiry_loop(self) {
        let mut ticker = tokio::time::interval(SESSION_EXPIRY_INTERVAL);
        loop {
            ticker.tick().await;
            self.expire_sessions(now_ms(), self.log.session_expiry)
                .await;
        }
    }

    /// Touches connected sessions, then removes sessions idle longer than `expiry`.
    ///
    /// Args:
    ///     now_ms: The current time.
    ///     expiry: How long an idle session is kept.
    ///
    /// Returns:
    ///     How many sessions were removed.
    pub async fn expire_sessions(&self, now_ms: i64, expiry: Duration) -> u64 {
        for session in self.with(|broker| broker.connected_sessions()) {
            if let Err(e) = self.storage.touch(&session, now_ms).await {
                warn!(%e, "touching a connected session failed");
            }
        }
        let cutoff = now_ms.saturating_sub(i64::try_from(expiry.as_millis()).unwrap_or(i64::MAX));
        match self.storage.expire(cutoff).await {
            Ok(removed) => {
                if removed > 0 {
                    info!(removed, "expired idle sessions");
                }
                removed
            }
            Err(e) => {
                warn!(%e, "expiring sessions failed");
                0
            }
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
            let retention = self.retention_for(&topic);
            match self.storage.trim(&topic, &retention, now_ms).await {
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
