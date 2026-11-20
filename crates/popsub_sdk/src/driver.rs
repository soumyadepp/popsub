//! The driver: one task per `Client` that owns the WebSocket and does
//! everything the application shouldn't have to.
//!
//! It connects, logs in and authenticates, then reads frames and the
//! client's commands until the connection ends. When it ends, the close code
//! decides what happens next: reconnect with jittered backoff, or stop for
//! good (`SessionTakenOver`, `UnsupportedProtocol`, rejected credentials).
//! After reconnecting it resubscribes every topic from the next `seq` it
//! expects (topics a durable session restored on its own are left to the
//! server), and resends confirmed publishes that never got their
//! `published`, under the same `message_id` so the server stores them once.
//!
//! Every message is checked against the next `seq` its subscription
//! expects, so a retry or a replay after reconnecting never reaches the
//! application twice. A duplicate the application already acknowledged (or
//! any duplicate in `AckMode::Auto`) is acknowledged again, so a lost ack
//! can't stall the subscription.
//!
//! A wildcard subscription (`orders/#`) is one `Pattern`; each topic the
//! server joins for it becomes a `Topic` owned by that pattern, with its
//! own next expected `seq`, so deduplication and gap-free resubscription
//! work per topic exactly as for a subscription by name. Messages from
//! every owned topic go to the pattern's one `Subscription`.
//!
//! A shared subscription (a topic read through a `group`) gets only some
//! of the topic's messages, out of order when another member's are handed
//! over, so it remembers the `seq`s it has delivered instead of a next
//! expected one, and after a reconnect rejoins the group with `resume`: the
//! group's position lives on the server.
//!
//! When the server ends a subscription on its own (an admin revoked the
//! user's access), it sends `unsubscribed` with no id. The driver then
//! drops the topic, which ends its `Subscription`, and reports
//! `Event::Unsubscribed`. Every unsubscribe the driver sends carries an id,
//! so its reply is never mistaken for that.
//!
//! While a subscription's buffer is full the driver waits, which stops it
//! reading the socket; the server then paces delivery for this connection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use popsub_protocol::{
    ClientFrame, ClientMessage, CloseCode, Delivery, Encoding, ErrorCode, Limits, PROTOCOL_VERSION,
    RequestId, ServerFrame, ServerMessage, StartFrom, Subscribed,
};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
};
use tracing::{debug, info, warn};

use crate::config::{AckMode, ClientConfig, Credentials};
use crate::error::Error;
use crate::message::{Event, Message};

/// The client side of a WebSocket connection.
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How many acknowledged `seq`s a subscription remembers, to re-acknowledge
/// duplicates the application already acknowledged.
const ACK_MEMORY: usize = 4096;

/// How many QoS 0 publishes are held while disconnected; older ones are
/// dropped first.
const OFFLINE_PUBLISHES: usize = 10_000;

/// How long the driver waits for something from the server before deciding
/// the connection is dead, before the server has announced its own limits.
const DEFAULT_SILENCE: Duration = Duration::from_secs(120);

/// The result of a confirmed publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The message's position in the topic.
    pub seq: u64,
    /// The id it was stored under.
    pub message_id: String,
}

/// What the `Client` asks the driver to do.
#[derive(Debug)]
pub(crate) enum Command {
    /// Subscribe and report the `subscribed` frame.
    Subscribe {
        /// The topic.
        topic: String,
        /// Where replay starts.
        from: StartFrom,
        /// The shared subscription to join, if any.
        group: Option<String>,
        /// Identifies this subscription, so a stale `Forget` can't remove a newer one.
        generation: u64,
        /// Where the topic's messages go.
        buffer: mpsc::Sender<Message>,
        /// Where the outcome goes.
        reply: oneshot::Sender<Result<Subscribed, Error>>,
    },
    /// Unsubscribe and report `unsubscribed`.
    Unsubscribe {
        /// The topic.
        topic: String,
        /// Where the outcome goes.
        reply: oneshot::Sender<Result<(), Error>>,
    },
    /// Unsubscribe without waiting, when a `Subscription` is dropped.
    Forget {
        /// The topic.
        topic: String,
        /// The subscription being dropped.
        generation: u64,
    },
    /// Publish a message.
    Publish {
        /// The topic.
        topic: String,
        /// The body.
        payload: String,
        /// How the body encodes the message's bytes; `None` means it is text.
        encoding: Option<Encoding>,
        /// For a confirmed (QoS 1) publish, the idempotency key and where the
        /// outcome goes. `None` is a QoS 0 publish.
        confirm: Option<(String, oneshot::Sender<Result<Published, Error>>)>,
    },
    /// Acknowledge a message (from `Message::ack`).
    Ack {
        /// The topic.
        topic: String,
        /// The message's `seq`.
        seq: u64,
    },
    /// Send a request that is neither a subscribe, a publish nor an ack
    /// (the admin requests), and report its reply. Never queued while
    /// disconnected and never resent, because it may not be safe to repeat.
    Request {
        /// The request.
        message: ClientMessage,
        /// Where the reply goes; an `error` frame arrives as `Error::Server`.
        reply: oneshot::Sender<Result<ServerMessage, Error>>,
    },
    /// Close the connection and stop.
    Close,
}

/// The `seq`s a subscription has acknowledged recently.
#[derive(Debug, Default)]
struct AckMemory {
    /// The acknowledged `seq`s.
    seen: HashSet<u64>,
    /// The same `seq`s, oldest first, for forgetting the oldest.
    order: VecDeque<u64>,
}

impl AckMemory {
    /// Remembers an acknowledged `seq`, forgetting the oldest beyond `ACK_MEMORY`.
    ///
    /// Args:
    ///     seq: The acknowledged message.
    fn insert(&mut self, seq: u64) {
        if self.seen.insert(seq) {
            self.order.push_back(seq);
            if self.order.len() > ACK_MEMORY
                && let Some(oldest) = self.order.pop_front()
            {
                self.seen.remove(&oldest);
            }
        }
    }

    /// Whether a `seq` was acknowledged.
    ///
    /// Args:
    ///     seq: The message.
    ///
    /// Returns:
    ///     True if it is remembered as acknowledged.
    fn contains(&self, seq: u64) -> bool {
        self.seen.contains(&seq)
    }
}

/// One subscribed topic.
#[derive(Debug)]
struct Topic {
    /// Where its messages go.
    buffer: mpsc::Sender<Message>,
    /// Where replay started when the application subscribed.
    from: StartFrom,
    /// The next `seq` the application should see. Anything lower is a duplicate.
    next_expected: Option<u64>,
    /// Which subscription this is.
    generation: u64,
    /// What it has acknowledged.
    acked: AckMemory,
    /// The wildcard subscription this topic was joined through, if any.
    owner: Option<String>,
    /// The shared subscription this topic is read through, if any. Its
    /// messages arrive out of order and with gaps (other members get the
    /// rest), so duplicates are found with `seen` instead of `next_expected`.
    group: Option<String>,
    /// The `seq`s already handed to the application, for a shared subscription.
    seen: AckMemory,
    /// Whether the server held this topic for the session before the
    /// application subscribed, and the `subscribed` that answers the
    /// application's subscribe hasn't arrived yet. Until it does, messages
    /// on the topic are from the server's own resume: they are dropped
    /// without an acknowledgement, because the subscribe replays them.
    claiming: bool,
}

/// One wildcard subscription. The topics it brings in are `Topic`s owned by it.
#[derive(Debug)]
struct Pattern {
    /// Where the messages of every topic it matches go.
    buffer: mpsc::Sender<Message>,
    /// Which subscription this is.
    generation: u64,
}

/// A request waiting for its reply, by request id.
#[derive(Debug)]
enum Pending {
    /// A subscribe; the waiter is in `Driver::subscribe_waiters`.
    Subscribe {
        /// The topic.
        topic: String,
    },
    /// An unsubscribe.
    Unsubscribe {
        /// Where the outcome goes.
        reply: oneshot::Sender<Result<(), Error>>,
    },
    /// A confirmed publish; its frame is in `Driver::unconfirmed` for resending.
    Publish {
        /// Where the outcome goes.
        reply: oneshot::Sender<Result<Published, Error>>,
    },
    /// A request from `Command::Request`.
    Request {
        /// Where the reply goes.
        reply: oneshot::Sender<Result<ServerMessage, Error>>,
    },
    /// A login to refresh the token on the current connection.
    Relogin,
    /// The `auth` that installs the refreshed token.
    Reauth,
}

/// How a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The application closed the client.
    Stop,
    /// The connection was lost, with the server's close code if it sent one.
    Lost(Option<CloseCode>),
}

/// Why connecting failed.
#[derive(Debug)]
struct ConnectFailure {
    /// The error to report.
    error: Error,
    /// Whether retrying could help.
    retry: bool,
}

/// The driver's state.
pub(crate) struct Driver {
    /// How to connect.
    config: ClientConfig,
    /// Commands from the `Client`.
    commands: mpsc::UnboundedReceiver<Command>,
    /// A weak handle on the command queue, for `Message::ack`. Weak, so the
    /// driver stops when every `Client` and `Subscription` is dropped.
    acker: mpsc::WeakUnboundedSender<Command>,
    /// Where events go.
    events: broadcast::Sender<Event>,
    /// Why the client stopped, once it has.
    closed: Arc<OnceLock<Option<CloseCode>>>,
    /// Subscribed topics.
    topics: HashMap<String, Topic>,
    /// Wildcard subscriptions, by pattern.
    patterns: HashMap<String, Pattern>,
    /// Topics the server restored for the session that the application
    /// hasn't subscribed to in this process. Their messages are dropped
    /// without an acknowledgement, so the server keeps them for the session.
    unclaimed: HashSet<String>,
    /// Whether the server left the session's topics unattached on this
    /// connection (`authenticated.held`). Then nothing arrives on a topic
    /// until this client subscribes to it, so nothing is unclaimed, and
    /// `restore` subscribes to the resumed topics itself.
    held: bool,
    /// Subscribes waiting for `subscribed`, by topic.
    subscribe_waiters: HashMap<String, oneshot::Sender<Result<Subscribed, Error>>>,
    /// Requests waiting for a reply, by id.
    pending: HashMap<u64, Pending>,
    /// Confirmed publishes not yet confirmed, by request id, for resending.
    unconfirmed: HashMap<u64, ClientMessage>,
    /// Frames to send once connected (QoS 0 publishes, unsubscribes).
    offline: VecDeque<ClientMessage>,
    /// The next request id.
    next_id: u64,
    /// Whether a connection has ever succeeded, so `clean_start` is only sent once.
    connected_once: bool,
    /// How long to wait for anything from the server before giving up on
    /// the connection.
    silence: Duration,
    /// The TLS setup for `wss://` URLs, built once.
    tls: Connector,
}

impl Driver {
    /// Builds a driver that hasn't connected yet.
    ///
    /// Args:
    ///     config: How to connect.
    ///     commands: Commands from the `Client`.
    ///     acker: A weak handle on the command queue.
    ///     events: Where events go.
    ///     closed: Set to the final close code when the client stops.
    ///
    /// Returns:
    ///     The driver.
    pub(crate) fn new(
        config: ClientConfig,
        commands: mpsc::UnboundedReceiver<Command>,
        acker: mpsc::WeakUnboundedSender<Command>,
        events: broadcast::Sender<Event>,
        closed: Arc<OnceLock<Option<CloseCode>>>,
    ) -> Self {
        Self {
            commands,
            acker,
            events,
            closed,
            topics: HashMap::new(),
            patterns: HashMap::new(),
            unclaimed: HashSet::new(),
            held: false,
            subscribe_waiters: HashMap::new(),
            pending: HashMap::new(),
            unconfirmed: HashMap::new(),
            offline: VecDeque::new(),
            next_id: 1,
            connected_once: false,
            silence: DEFAULT_SILENCE,
            tls: tls_connector(&config.extra_roots),
            config,
        }
    }

    /// Runs until the client stops: connects, serves the connection,
    /// reconnects with backoff, and reports the first connection's outcome.
    ///
    /// Args:
    ///     ready: Gets `Ok` when the first connection is up, or the error
    ///         that stopped the client before it ever connected.
    pub(crate) async fn run(mut self, ready: oneshot::Sender<Result<(), Error>>) {
        let mut ready = Some(ready);
        let mut attempt: u32 = 0;
        let mut last_error: Option<Error> = None;
        let final_code = loop {
            match self.connect().await {
                Ok((socket, resumed)) => {
                    attempt = 0;
                    self.connected_once = true;
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Ok(()));
                    }
                    let _ = self.events.send(Event::Connected {
                        resumed: resumed.clone(),
                    });
                    match self.serve(socket, resumed).await {
                        Ending::Stop => break None,
                        Ending::Lost(code) => {
                            self.fail_requests();
                            let reconnecting = self.should_reconnect(code);
                            let _ = self.events.send(Event::Disconnected { code, reconnecting });
                            if !reconnecting {
                                break code;
                            }
                        }
                    }
                }
                Err(failure) => {
                    warn!(error = %failure.error, "connecting failed");
                    last_error = Some(failure.error.clone());
                    if !failure.retry {
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(Err(failure.error));
                        }
                        break None;
                    }
                }
            }
            attempt += 1;
            if self
                .config
                .reconnect
                .max_attempts
                .is_some_and(|max| attempt > max)
            {
                if let Some(ready) = ready.take() {
                    let reason = last_error
                        .as_ref()
                        .map_or_else(String::new, |e| format!("; the last attempt failed: {e}"));
                    let _ = ready.send(Err(Error::Connect(format!(
                        "ran out of reconnect attempts{reason}"
                    ))));
                }
                break None;
            }
            if !self.wait_before_retry(attempt).await {
                break None;
            }
        };
        self.stop(final_code);
    }

    /// Decides whether to reconnect after a connection was lost.
    ///
    /// Args:
    ///     code: The server's close code, if it sent one.
    ///
    /// Returns:
    ///     True to reconnect. An expired token only allows it with password
    ///     credentials, which log in again.
    fn should_reconnect(&self, code: Option<CloseCode>) -> bool {
        match code {
            None => true,
            Some(CloseCode::TokenExpired) => {
                matches!(self.config.credentials, Credentials::Password { .. })
            }
            Some(code) => code.should_reconnect(),
        }
    }

    /// Waits before the next connection attempt, queueing commands meanwhile.
    ///
    /// Args:
    ///     attempt: How many attempts in a row have failed.
    ///
    /// Returns:
    ///     False when the application closed the client while waiting.
    async fn wait_before_retry(&mut self, attempt: u32) -> bool {
        let ceiling = self.config.reconnect.ceiling(attempt);
        let delay = ceiling.mul_f64(rand::random::<f64>());
        debug!(?delay, attempt, "waiting before reconnecting");
        let until = Instant::now() + delay;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return true,
                command = self.commands.recv() => match command {
                    None | Some(Command::Close) => return false,
                    Some(command) => self.queue_offline(command),
                },
            }
        }
    }

    /// Fails every `Command::Request` still waiting when a connection is
    /// lost. The server may or may not have carried it out.
    fn fail_requests(&mut self) {
        let waiting: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| matches!(pending, Pending::Request { .. }))
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            if let Some(Pending::Request { reply }) = self.pending.remove(&id) {
                let _ = reply.send(Err(Error::Connect(
                    "the connection dropped before the reply; the request may or may not have been carried out".to_string(),
                )));
            }
        }
    }

    /// Takes a request id for a frame the driver sends on its own.
    ///
    /// An unsubscribe always gets one, so its `unsubscribed` reply can't be
    /// mistaken for the server ending the subscription, which it announces
    /// with no id.
    ///
    /// Args:
    ///     message: The frame.
    ///
    /// Returns:
    ///     An id for an unsubscribe, otherwise `None`.
    fn quiet_id(&mut self, message: &ClientMessage) -> Option<u64> {
        matches!(message, ClientMessage::Unsubscribe { .. }).then(|| self.take_id())
    }

    /// Takes a request id.
    ///
    /// Returns:
    ///     A new id.
    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Opens a connection, logs in if needed, and authenticates.
    ///
    /// Returns:
    ///     The socket and the topics the server restored for the session.
    ///
    /// Raises:
    ///     ConnectFailure: With `retry` false for rejected credentials, an
    ///         unsupported protocol or a bad session id, and true otherwise.
    async fn connect(&mut self) -> Result<(Socket, Vec<String>), ConnectFailure> {
        let retry = |error: Error| ConnectFailure { error, retry: true };
        let (mut socket, _) = connect_async_tls_with_config(
            self.config.url.as_str(),
            None,
            false,
            Some(self.tls.clone()),
        )
        .await
        .map_err(|e| retry(Error::Connect(e.to_string())))?;
        let token = match self.config.credentials.clone() {
            Credentials::Token(token) => token,
            Credentials::Password { username, password } => {
                let id = self.take_id();
                match self
                    .exchange(&mut socket, id, ClientMessage::Login { username, password })
                    .await?
                {
                    ServerMessage::LoggedIn { token } => token,
                    other => return Err(refusal(other)),
                }
            }
        };
        let id = self.take_id();
        let auth = ClientMessage::Auth {
            token,
            protocol: Some(PROTOCOL_VERSION),
            session_id: self.config.session_id.clone(),
            clean_start: self.config.clean_start && !self.connected_once,
            hold_resumed: self.config.session_id.is_some(),
        };
        match self.exchange(&mut socket, id, auth).await? {
            ServerMessage::Authenticated {
                resumed,
                limits,
                held,
                ..
            } => {
                self.silence = silence_limit(&limits);
                self.held = held;
                info!(url = %self.config.url, resumed = resumed.len(), held, "connected");
                Ok((socket, resumed))
            }
            other => Err(refusal(other)),
        }
    }

    /// Sends a request during connecting and waits for its reply.
    ///
    /// Args:
    ///     socket: The connection.
    ///     id: The request id.
    ///     message: The request.
    ///
    /// Returns:
    ///     The reply.
    ///
    /// Raises:
    ///     ConnectFailure: When the connection drops or closes first, or no
    ///         reply arrives within the request timeout.
    async fn exchange(
        &self,
        socket: &mut Socket,
        id: u64,
        message: ClientMessage,
    ) -> Result<ServerMessage, ConnectFailure> {
        let lost = |error: Error| ConnectFailure { error, retry: true };
        send(socket, Some(id), message)
            .await
            .map_err(|_| lost(Error::Connect("the connection dropped".to_string())))?;
        let wait = async {
            while let Some(frame) = socket.next().await {
                match frame {
                    Ok(WsMessage::Text(text)) => {
                        if let Ok(frame) = serde_json::from_str::<ServerFrame>(&text)
                            && frame.id == Some(RequestId::Number(id))
                        {
                            return Ok(frame.message);
                        }
                    }
                    Ok(WsMessage::Close(close)) => {
                        let code = close.and_then(|c| CloseCode::from_code(c.code.into()));
                        return Err(ConnectFailure {
                            error: Error::Closed(code),
                            retry: code.is_none_or(|c| c.should_reconnect()),
                        });
                    }
                    Ok(_) => {}
                    Err(e) => return Err(lost(Error::Connect(e.to_string()))),
                }
            }
            Err(lost(Error::Connect("the connection closed".to_string())))
        };
        tokio::time::timeout(self.config.request_timeout, wait)
            .await
            .unwrap_or_else(|_| Err(lost(Error::Timeout)))
    }

    /// Serves one connection until it ends.
    ///
    /// Restores subscriptions and resends unconfirmed publishes and queued
    /// frames first, then handles frames and commands.
    ///
    /// Args:
    ///     socket: The authenticated connection.
    ///     resumed: Topics the server restored for the session.
    ///
    /// Returns:
    ///     How the connection ended.
    async fn serve(&mut self, mut socket: Socket, resumed: Vec<String>) -> Ending {
        self.unclaimed = resumed
            .iter()
            .filter(|name| !self.held && !self.topics.contains_key(*name))
            .cloned()
            .collect();
        if self.restore(&mut socket, &resumed).await.is_err() {
            return Ending::Lost(None);
        }
        let mut last_heard = Instant::now();
        loop {
            tokio::select! {
                frame = socket.next() => {
                    last_heard = Instant::now();
                    match frame {
                        Some(Ok(WsMessage::Text(text))) => {
                            match serde_json::from_str::<ServerFrame>(&text) {
                                Ok(frame) => {
                                    if self.on_frame(&mut socket, frame).await.is_err() {
                                        return Ending::Lost(None);
                                    }
                                }
                                Err(e) => debug!(%e, "ignoring a frame this client doesn't know"),
                            }
                        }
                        Some(Ok(WsMessage::Close(close))) => {
                            return Ending::Lost(close.and_then(|c| CloseCode::from_code(c.code.into())));
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            debug!(%e, "connection error");
                            return Ending::Lost(None);
                        }
                        None => return Ending::Lost(None),
                    }
                }
                command = self.commands.recv() => match command {
                    None | Some(Command::Close) => {
                        let _ = socket.close(None).await;
                        return Ending::Stop;
                    }
                    Some(command) => {
                        if self.on_command(&mut socket, command).await.is_err() {
                            return Ending::Lost(None);
                        }
                    }
                },
                _ = tokio::time::sleep_until(last_heard + self.silence) => {
                    warn!("no word from the server, reconnecting");
                    return Ending::Lost(None);
                }
            }
        }
    }

    /// Restores state on a new connection: resubscribes, resends
    /// unconfirmed publishes, sends queued frames.
    ///
    /// When the server holds the session's topics (`held`), each one this
    /// client tracks is subscribed again from `Resume`, which is one past
    /// the session's cursor. Otherwise topics the server restored for the
    /// session are left alone, except
    /// one still waiting for the `subscribed` that claims it, which is
    /// subscribed again the way the application asked; every other topic is subscribed again from the next `seq` it expects, so
    /// nothing still in the log is missed. Wildcard subscriptions are sent
    /// again after that, from `seq` 1: the server only joins topics the
    /// connection doesn't hold yet, which are exactly the ones created while
    /// it was away, so they are read from their first message.
    ///
    /// Args:
    ///     socket: The connection.
    ///     resumed: Topics the server restored.
    ///
    /// Raises:
    ///     (): When a write fails.
    async fn restore(&mut self, socket: &mut Socket, resumed: &[String]) -> Result<(), ()> {
        let mut resubscribe = Vec::new();
        for (name, topic) in &self.topics {
            if topic.group.is_some() {
                resubscribe.push((name.clone(), StartFrom::Resume, topic.group.clone()));
            } else if topic.claiming {
                let from = match topic.owner {
                    Some(_) => StartFrom::Resume,
                    None => topic.from,
                };
                resubscribe.push((name.clone(), from, None));
            } else if !resumed.contains(name) {
                let from = topic.next_expected.map_or(topic.from, StartFrom::Seq);
                resubscribe.push((name.clone(), from, None));
            } else if self.held {
                resubscribe.push((name.clone(), StartFrom::Resume, None));
            }
        }
        for (name, from, group) in resubscribe {
            let id = self.take_id();
            self.pending.insert(
                id,
                Pending::Subscribe {
                    topic: name.clone(),
                },
            );
            send(
                socket,
                Some(id),
                ClientMessage::Subscribe {
                    topic: name,
                    from,
                    group,
                },
            )
            .await?;
        }
        let mut patterns: Vec<String> = self.patterns.keys().cloned().collect();
        patterns.sort();
        for pattern in patterns {
            let id = self.take_id();
            self.pending.insert(
                id,
                Pending::Subscribe {
                    topic: pattern.clone(),
                },
            );
            let again = ClientMessage::Subscribe {
                topic: pattern,
                from: StartFrom::Seq(1),
                group: None,
            };
            send(socket, Some(id), again).await?;
        }
        let mut unconfirmed: Vec<(u64, ClientMessage)> = self
            .unconfirmed
            .iter()
            .map(|(id, message)| (*id, message.clone()))
            .collect();
        unconfirmed.sort_by_key(|(id, _)| *id);
        for (id, message) in unconfirmed {
            send(socket, Some(id), message).await?;
        }
        while let Some(message) = self.offline.pop_front() {
            let id = self.quiet_id(&message);
            send(socket, id, message).await?;
        }
        Ok(())
    }

    /// Records a command that arrived while disconnected, to act on after
    /// reconnecting.
    ///
    /// Args:
    ///     command: The command.
    fn queue_offline(&mut self, command: Command) {
        match command {
            Command::Subscribe {
                topic,
                from,
                group,
                generation,
                buffer,
                reply,
            } => {
                self.add_topic(topic.clone(), from, group, generation, buffer);
                self.subscribe_waiters.insert(topic, reply);
            }
            Command::Unsubscribe { topic, reply } => {
                let mut names = self.release(&topic, None);
                if names.is_empty() {
                    names.push(topic);
                }
                for name in names {
                    self.offline
                        .push_back(ClientMessage::Unsubscribe { topic: name });
                }
                let _ = reply.send(Ok(()));
            }
            Command::Forget { topic, generation } => {
                for name in self.release(&topic, Some(generation)) {
                    self.offline
                        .push_back(ClientMessage::Unsubscribe { topic: name });
                }
            }
            Command::Publish {
                topic,
                payload,
                encoding,
                confirm,
            } => {
                self.queue_publish(topic, payload, encoding, confirm);
            }
            Command::Ack { topic, seq } => {
                if let Some(entry) = self.topics.get_mut(&topic) {
                    entry.acked.insert(seq);
                }
            }
            Command::Request { reply, .. } => {
                let _ = reply.send(Err(Error::Connect(
                    "not connected; this request isn't queued while disconnected".to_string(),
                )));
            }
            Command::Close => {}
        }
    }

    /// Registers a publish: a confirmed one waits in `unconfirmed` for its
    /// `published`, a QoS 0 one waits in `offline` until it is sent.
    ///
    /// Args:
    ///     topic: The topic.
    ///     payload: The body.
    ///     encoding: How the body encodes the message's bytes, if it isn't text.
    ///     confirm: The idempotency key and reply for a confirmed publish.
    ///
    /// Returns:
    ///     The request id of a confirmed publish.
    fn queue_publish(
        &mut self,
        topic: String,
        payload: String,
        encoding: Option<Encoding>,
        confirm: Option<(String, oneshot::Sender<Result<Published, Error>>)>,
    ) -> Option<u64> {
        match confirm {
            Some((message_id, reply)) => {
                let id = self.take_id();
                self.pending.insert(id, Pending::Publish { reply });
                self.unconfirmed.insert(
                    id,
                    ClientMessage::Publish {
                        topic,
                        payload,
                        encoding,
                        message_id: Some(message_id),
                        qos: Some(1),
                    },
                );
                Some(id)
            }
            None => {
                if self.offline.len() >= OFFLINE_PUBLISHES {
                    self.offline.pop_front();
                    warn!("dropped the oldest publish held while disconnected");
                }
                self.offline.push_back(ClientMessage::Publish {
                    topic,
                    payload,
                    encoding,
                    message_id: None,
                    qos: None,
                });
                None
            }
        }
    }

    /// Adds or replaces a subscription: a topic, or a wildcard pattern.
    ///
    /// Subscribing again to a pattern the client holds replaces the
    /// subscription: the topics it owned move to the new one and keep the
    /// next `seq` they expect, because the server sends nothing again for
    /// topics the connection holds. Subscribing again to a topic by name
    /// starts over from `from`.
    ///
    /// A topic the server held for the session, subscribed by name or
    /// matched by the pattern, is claimed: it is tracked with `claiming`
    /// set, so what the server sent on its own is dropped until the
    /// `subscribed` that answers the claim. A topic subscribed again while
    /// its claim is unanswered stays `claiming`.
    ///
    /// Args:
    ///     topic: The topic or pattern.
    ///     from: Where replay starts.
    ///     group: The shared subscription to join, if any.
    ///     generation: Which subscription this is.
    ///     buffer: Where its messages go.
    ///
    /// Returns:
    ///     For a pattern, the session's topics it claimed, which the caller
    ///     subscribes to by name with `StartFrom::Resume`. Empty for a topic.
    fn add_topic(
        &mut self,
        topic: String,
        from: StartFrom,
        group: Option<String>,
        generation: u64,
        buffer: mpsc::Sender<Message>,
    ) -> Vec<String> {
        if popsub_protocol::topics::is_pattern(&topic) {
            for owned in self.topics.values_mut() {
                if owned.owner.as_deref() == Some(topic.as_str()) {
                    owned.buffer = buffer.clone();
                    owned.generation = generation;
                }
            }
            let mut claimed: Vec<String> = self
                .unclaimed
                .iter()
                .filter(|name| popsub_protocol::topics::matches_pattern(&topic, name))
                .cloned()
                .collect();
            claimed.sort();
            for name in &claimed {
                self.unclaimed.remove(name);
                self.topics.insert(
                    name.clone(),
                    Topic {
                        buffer: buffer.clone(),
                        from: StartFrom::Resume,
                        next_expected: None,
                        generation,
                        acked: AckMemory::default(),
                        owner: Some(topic.clone()),
                        group: None,
                        seen: AckMemory::default(),
                        claiming: true,
                    },
                );
            }
            self.patterns.insert(topic, Pattern { buffer, generation });
            return claimed;
        }
        let claiming = self.unclaimed.remove(&topic)
            || self.topics.get(&topic).is_some_and(|held| held.claiming);
        self.topics.insert(
            topic,
            Topic {
                buffer,
                from,
                next_expected: None,
                generation,
                acked: AckMemory::default(),
                owner: None,
                group,
                seen: AckMemory::default(),
                claiming,
            },
        );
        Vec::new()
    }

    /// Starts tracking a topic the server joined for one of this client's
    /// wildcard subscriptions. When several match, the oldest gets it.
    ///
    /// Args:
    ///     name: The topic.
    ///     start: The first `seq` the server will send, when known.
    ///
    /// Returns:
    ///     Whether a wildcard subscription matched.
    fn adopt(&mut self, name: &str, start: Option<u64>) -> bool {
        let Some((pattern, owner)) = self
            .patterns
            .iter()
            .filter(|(pattern, _)| popsub_protocol::topics::matches_pattern(pattern, name))
            .min_by_key(|(_, owner)| owner.generation)
        else {
            return false;
        };
        let topic = Topic {
            buffer: owner.buffer.clone(),
            from: start.map_or(StartFrom::Seq(1), StartFrom::Seq),
            next_expected: start,
            generation: owner.generation,
            acked: AckMemory::default(),
            owner: Some(pattern.clone()),
            group: None,
            seen: AckMemory::default(),
            claiming: false,
        };
        self.topics.insert(name.to_string(), topic);
        true
    }

    /// Stops tracking a subscription: a topic subscribed by name, or a
    /// wildcard pattern together with every topic joined through it.
    ///
    /// Args:
    ///     name: The topic or pattern.
    ///     generation: Only release it if it is still this subscription;
    ///         `None` releases whatever is there.
    ///
    /// Returns:
    ///     What to unsubscribe from on the server: the name, and for a
    ///     pattern the topics it owned. Empty when nothing was released.
    fn release(&mut self, name: &str, generation: Option<u64>) -> Vec<String> {
        let same = |held: u64| generation.is_none_or(|wanted| wanted == held);
        if self.patterns.get(name).is_some_and(|p| same(p.generation)) {
            self.patterns.remove(name);
            let mut names = vec![name.to_string()];
            let owned: Vec<String> = self
                .topics
                .iter()
                .filter(|(_, topic)| topic.owner.as_deref() == Some(name))
                .map(|(topic, _)| topic.clone())
                .collect();
            for topic in owned {
                self.topics.remove(&topic);
                names.push(topic);
            }
            return names;
        }
        let by_name = self
            .topics
            .get(name)
            .is_some_and(|t| t.owner.is_none() && same(t.generation));
        if by_name {
            self.topics.remove(name);
            return vec![name.to_string()];
        }
        Vec::new()
    }

    /// Acts on a command while connected.
    ///
    /// Args:
    ///     socket: The connection.
    ///     command: The command.
    ///
    /// Raises:
    ///     (): When a write fails.
    async fn on_command(&mut self, socket: &mut Socket, command: Command) -> Result<(), ()> {
        match command {
            Command::Subscribe {
                topic,
                from,
                group,
                generation,
                buffer,
                reply,
            } => {
                let claimed =
                    self.add_topic(topic.clone(), from, group.clone(), generation, buffer);
                self.subscribe_waiters.insert(topic.clone(), reply);
                let id = self.take_id();
                self.pending.insert(
                    id,
                    Pending::Subscribe {
                        topic: topic.clone(),
                    },
                );
                let subscribe = ClientMessage::Subscribe { topic, from, group };
                send(socket, Some(id), subscribe).await?;
                for name in claimed {
                    let id = self.take_id();
                    self.pending.insert(
                        id,
                        Pending::Subscribe {
                            topic: name.clone(),
                        },
                    );
                    let claim = ClientMessage::Subscribe {
                        topic: name,
                        from: StartFrom::Resume,
                        group: None,
                    };
                    send(socket, Some(id), claim).await?;
                }
                Ok(())
            }
            Command::Unsubscribe { topic, reply } => {
                let mut names = self.release(&topic, None);
                if names.is_empty() {
                    names.push(topic);
                }
                let mut reply = Some(reply);
                for name in names {
                    let id = self.take_id();
                    if let Some(reply) = reply.take() {
                        self.pending.insert(id, Pending::Unsubscribe { reply });
                    }
                    send(socket, Some(id), ClientMessage::Unsubscribe { topic: name }).await?;
                }
                Ok(())
            }
            Command::Forget { topic, generation } => {
                for name in self.release(&topic, Some(generation)) {
                    let id = self.take_id();
                    send(socket, Some(id), ClientMessage::Unsubscribe { topic: name }).await?;
                }
                Ok(())
            }
            Command::Publish {
                topic,
                payload,
                encoding,
                confirm,
            } => match self.queue_publish(topic, payload, encoding, confirm) {
                Some(id) => {
                    let message = self.unconfirmed[&id].clone();
                    send(socket, Some(id), message).await
                }
                None => {
                    while let Some(message) = self.offline.pop_front() {
                        let id = self.quiet_id(&message);
                        send(socket, id, message).await?;
                    }
                    Ok(())
                }
            },
            Command::Ack { topic, seq } => {
                if let Some(entry) = self.topics.get_mut(&topic) {
                    entry.acked.insert(seq);
                }
                send(socket, None, ClientMessage::Ack { topic, seq }).await
            }
            Command::Request { message, reply } => {
                let id = self.take_id();
                self.pending.insert(id, Pending::Request { reply });
                send(socket, Some(id), message).await
            }
            Command::Close => Ok(()),
        }
    }

    /// Acts on a frame from the server.
    ///
    /// Args:
    ///     socket: The connection.
    ///     frame: The frame.
    ///
    /// Raises:
    ///     (): When a write fails.
    async fn on_frame(&mut self, socket: &mut Socket, frame: ServerFrame) -> Result<(), ()> {
        let pending = match &frame.id {
            Some(RequestId::Number(id)) => self.pending.remove(id).map(|p| (*id, p)),
            _ => None,
        };
        let pending = match pending {
            Some((_, Pending::Request { reply })) => {
                let outcome = match frame.message {
                    ServerMessage::Error {
                        code,
                        message,
                        retry_after,
                    } => Err(Error::Server {
                        code,
                        message,
                        retry_after,
                    }),
                    other => Ok(other),
                };
                let _ = reply.send(outcome);
                return Ok(());
            }
            other => other,
        };
        let unprompted = frame.id.is_none();
        match frame.message {
            ServerMessage::Message(delivery) => self.deliver(socket, delivery).await?,
            ServerMessage::Subscribed(subscribed) => {
                let start = subscribed.replay_from;
                if self.patterns.contains_key(&subscribed.topic) {
                    debug!(pattern = %subscribed.topic, "wildcard subscription accepted");
                } else if let Some(topic) = self.topics.get_mut(&subscribed.topic) {
                    if topic.claiming {
                        topic.claiming = false;
                        topic.next_expected = None;
                    }
                    if topic.group.is_none() {
                        topic.next_expected =
                            Some(topic.next_expected.map_or(start, |n| n.max(start)));
                    }
                } else {
                    self.adopt(&subscribed.topic, Some(start));
                }
                if let Some(waiter) = self.subscribe_waiters.remove(&subscribed.topic) {
                    let _ = waiter.send(Ok(subscribed));
                }
            }
            ServerMessage::Unsubscribed { topic } => {
                if let Some((_, Pending::Unsubscribe { reply })) = pending {
                    let _ = reply.send(Ok(()));
                } else if unprompted && self.topics.remove(&topic).is_some() {
                    warn!(topic, "the server ended a subscription");
                    let _ = self.events.send(Event::Unsubscribed { topic });
                }
            }
            ServerMessage::Published { message_id, seq } => {
                if let Some((id, Pending::Publish { reply })) = pending {
                    self.unconfirmed.remove(&id);
                    let _ = reply.send(Ok(Published { seq, message_id }));
                }
            }
            ServerMessage::Truncated(truncated) => {
                if let Some(topic) = self.topics.get_mut(&truncated.topic)
                    && topic.group.is_none()
                {
                    let first = truncated.first_available;
                    topic.next_expected = Some(topic.next_expected.map_or(first, |n| n.max(first)));
                }
                let _ = self.events.send(Event::Truncated {
                    topic: truncated.topic,
                    first_available: truncated.first_available,
                });
            }
            ServerMessage::Stalled(stalled) => {
                let _ = self.events.send(Event::Stalled {
                    topic: stalled.topic,
                    seq: stalled.seq,
                });
            }
            ServerMessage::AuthExpiring { expires_at_ms } => {
                let _ = self.events.send(Event::AuthExpiring { expires_at_ms });
                if let Credentials::Password { username, password } =
                    self.config.credentials.clone()
                {
                    let id = self.take_id();
                    self.pending.insert(id, Pending::Relogin);
                    send(
                        socket,
                        Some(id),
                        ClientMessage::Login { username, password },
                    )
                    .await?;
                }
            }
            ServerMessage::LoggedIn { token } => {
                if matches!(pending, Some((_, Pending::Relogin))) {
                    let id = self.take_id();
                    self.pending.insert(id, Pending::Reauth);
                    let auth = ClientMessage::Auth {
                        token,
                        protocol: Some(PROTOCOL_VERSION),
                        session_id: None,
                        clean_start: false,
                        hold_resumed: false,
                    };
                    send(socket, Some(id), auth).await?;
                }
            }
            ServerMessage::Authenticated { .. } => {
                if matches!(pending, Some((_, Pending::Reauth))) {
                    let _ = self.events.send(Event::AuthRefreshed);
                }
            }
            ServerMessage::Error {
                code,
                message,
                retry_after,
            } => self.on_error(pending, code, message, retry_after),
            ServerMessage::Registered {}
            | ServerMessage::Pong {}
            | ServerMessage::User { .. }
            | ServerMessage::Users { .. }
            | ServerMessage::UserDeleted { .. }
            | ServerMessage::TopicInfo { .. }
            | ServerMessage::Topics { .. } => {}
        }
        Ok(())
    }

    /// Routes an error frame to the request it answers, or to the events.
    ///
    /// Args:
    ///     pending: The request it answers, if its `id` matched one.
    ///     code: The error code.
    ///     message: The error text.
    ///     retry_after: Seconds to wait, for limits.
    fn on_error(
        &mut self,
        pending: Option<(u64, Pending)>,
        code: ErrorCode,
        message: String,
        retry_after: Option<u64>,
    ) {
        let error = Error::Server {
            code,
            message: message.clone(),
            retry_after,
        };
        match pending {
            Some((_, Pending::Subscribe { topic })) => {
                self.release(&topic, None);
                if let Some(waiter) = self.subscribe_waiters.remove(&topic) {
                    let _ = waiter.send(Err(error));
                }
            }
            Some((_, Pending::Unsubscribe { reply })) => {
                let _ = reply.send(Err(error));
            }
            Some((id, Pending::Publish { reply })) => {
                self.unconfirmed.remove(&id);
                let _ = reply.send(Err(error));
            }
            Some((_, Pending::Request { reply })) => {
                let _ = reply.send(Err(error));
            }
            Some((_, Pending::Relogin | Pending::Reauth)) | None => {
                let _ = self.events.send(Event::ServerError { code, message });
            }
        }
    }

    /// Hands a delivered message to its subscription, dropping duplicates
    /// and acknowledging as `AckMode` says.
    ///
    /// A message on a topic the server holds for the session that the
    /// application hasn't subscribed to here, or whose subscribe is still
    /// unanswered, is dropped without an acknowledgement: the server keeps
    /// it and replays it when the application's subscribe is handled.
    ///
    /// Args:
    ///     socket: The connection, for acknowledgements.
    ///     delivery: The delivered message.
    ///
    /// Raises:
    ///     (): When a write fails.
    async fn deliver(&mut self, socket: &mut Socket, delivery: Delivery) -> Result<(), ()> {
        let ack = ClientMessage::Ack {
            topic: delivery.topic.clone(),
            seq: delivery.seq,
        };
        if self.unclaimed.contains(&delivery.topic) {
            return Ok(());
        }
        if !self.topics.contains_key(&delivery.topic) {
            self.adopt(&delivery.topic, None);
        }
        let Some(topic) = self.topics.get_mut(&delivery.topic) else {
            if delivery.qos == 1 {
                send(socket, None, ack).await?;
            }
            return Ok(());
        };
        if topic.claiming {
            return Ok(());
        }
        let duplicate = match topic.group {
            Some(_) => topic.seen.contains(delivery.seq),
            None => topic.next_expected.is_some_and(|next| delivery.seq < next),
        };
        if duplicate {
            let acked_before =
                self.config.ack_mode == AckMode::Auto || topic.acked.contains(delivery.seq);
            if delivery.qos == 1 && acked_before {
                send(socket, None, ack).await?;
            }
            return Ok(());
        }
        if topic.group.is_some() {
            topic.seen.insert(delivery.seq);
        } else {
            topic.next_expected = Some(delivery.seq + 1);
        }
        let mut acker = None;
        if delivery.qos == 1 {
            match self.config.ack_mode {
                AckMode::Auto => {
                    topic.acked.insert(delivery.seq);
                    send(socket, None, ack).await?;
                }
                AckMode::Manual => acker = self.acker.upgrade(),
            }
        }
        let buffer = topic.buffer.clone();
        let generation = topic.generation;
        let name = topic
            .owner
            .clone()
            .unwrap_or_else(|| delivery.topic.clone());
        let message = Message {
            topic: delivery.topic,
            seq: delivery.seq,
            message_id: delivery.message_id,
            payload: delivery.payload,
            encoding: delivery.encoding,
            qos: delivery.qos,
            published_at_ms: delivery.published_at_ms,
            acker,
        };
        if buffer.send(message).await.is_err() {
            for released in self.release(&name, Some(generation)) {
                let id = self.take_id();
                let leave = ClientMessage::Unsubscribe { topic: released };
                send(socket, Some(id), leave).await?;
            }
        }
        Ok(())
    }

    /// Stops the client for good: records why, and fails everything waiting.
    ///
    /// Args:
    ///     code: The close code that stopped it, if any.
    fn stop(&mut self, code: Option<CloseCode>) {
        let _ = self.closed.set(code);
        for (_, waiter) in self.subscribe_waiters.drain() {
            let _ = waiter.send(Err(Error::Closed(code)));
        }
        for (_, pending) in self.pending.drain() {
            match pending {
                Pending::Unsubscribe { reply } => {
                    let _ = reply.send(Err(Error::Closed(code)));
                }
                Pending::Publish { reply } => {
                    let _ = reply.send(Err(Error::Closed(code)));
                }
                Pending::Request { reply } => {
                    let _ = reply.send(Err(Error::Closed(code)));
                }
                Pending::Subscribe { .. } | Pending::Relogin | Pending::Reauth => {}
            }
        }
        self.topics.clear();
        self.patterns.clear();
        self.commands.close();
        while let Ok(command) = self.commands.try_recv() {
            fail_command(command, code);
        }
        info!(?code, "client stopped");
    }
}

/// Fails a command that arrived after the client stopped.
///
/// Args:
///     command: The command.
///     code: Why the client stopped.
fn fail_command(command: Command, code: Option<CloseCode>) {
    match command {
        Command::Subscribe { reply, .. } => {
            let _ = reply.send(Err(Error::Closed(code)));
        }
        Command::Unsubscribe { reply, .. } => {
            let _ = reply.send(Err(Error::Closed(code)));
        }
        Command::Publish {
            confirm: Some((_, reply)),
            ..
        } => {
            let _ = reply.send(Err(Error::Closed(code)));
        }
        Command::Request { reply, .. } => {
            let _ = reply.send(Err(Error::Closed(code)));
        }
        _ => {}
    }
}

/// Builds the TLS setup for `wss://`: the public web roots plus any the
/// application added, with `ring` for crypto.
///
/// Args:
///     extra_roots: Certificates to trust besides the public roots.
///
/// Returns:
///     The connector.
fn tls_connector(extra_roots: &[rustls::pki_types::CertificateDer<'static>]) -> Connector {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for root in extra_roots {
        if let Err(e) = roots.add(root.clone()) {
            warn!(%e, "ignoring a root certificate rustls can't use");
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Connector::Rustls(Arc::new(config))
}

/// Turns an unexpected reply during connecting into a failure.
///
/// Args:
///     reply: What the server sent instead of the expected reply.
///
/// Returns:
///     The failure; retryable only for the server's temporary refusals.
fn refusal(reply: ServerMessage) -> ConnectFailure {
    match reply {
        ServerMessage::Error {
            code,
            message,
            retry_after,
        } => {
            let error = Error::Server {
                code,
                message,
                retry_after,
            };
            ConnectFailure {
                retry: error.is_temporary(),
                error,
            }
        }
        other => ConnectFailure {
            error: Error::Protocol(format!("{other:?}")),
            retry: true,
        },
    }
}

/// How long to wait for anything from the server before giving up on the
/// connection: the server's idle timeout plus one heartbeat, since it pings
/// at that interval.
///
/// Args:
///     limits: The server's advertised limits.
///
/// Returns:
///     The limit.
fn silence_limit(limits: &Limits) -> Duration {
    Duration::from_secs(
        limits
            .idle_timeout_secs
            .max(1)
            .saturating_add(limits.heartbeat_secs),
    )
}

/// Sends one request.
///
/// Args:
///     socket: The connection.
///     id: The request id, if a reply is expected.
///     message: The request.
///
/// Raises:
///     (): When the write fails.
async fn send(socket: &mut Socket, id: Option<u64>, message: ClientMessage) -> Result<(), ()> {
    let frame = ClientFrame {
        id: id.map(RequestId::Number),
        message,
    };
    let text = serde_json::to_string(&frame).map_err(|_| ())?;
    socket
        .send(WsMessage::Text(text.into()))
        .await
        .map_err(|e| {
            debug!(%e, "write failed");
        })
}
