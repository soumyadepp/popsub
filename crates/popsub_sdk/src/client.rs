//! `Client`, the handle the application holds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use popsub_protocol::{ClientMessage, CloseCode, Encoding, ServerMessage, StartFrom, Subscribed};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::admin::Admin;
use crate::config::ClientConfig;
use crate::driver::{Command, Driver, Published};
use crate::error::Error;
use crate::message::{Event, Subscription};

/// How many events are kept for a slow `events()` receiver before it lags.
const EVENT_BUFFER: usize = 256;

/// A connection to a PopSub server that reconnects on its own.
///
/// Cloning is cheap and every clone drives the same connection. The
/// connection closes when `close` is called, or when every `Client` and
/// `Subscription` has been dropped.
#[derive(Debug, Clone)]
pub struct Client {
    /// The driver's command queue.
    commands: mpsc::UnboundedSender<Command>,
    /// Where the driver sends events.
    events: broadcast::Sender<Event>,
    /// Why the client stopped, once it has.
    closed: Arc<OnceLock<Option<CloseCode>>>,
    /// The next subscription generation.
    generations: Arc<AtomicU64>,
    /// How long requests may take, and the subscription buffer size.
    config: Arc<ClientConfig>,
}

impl Client {
    /// Connects to a server and starts the client.
    ///
    /// Waits for the first connection to log in and authenticate, retrying
    /// with the reconnect policy, up to `ClientConfig::request_timeout`.
    ///
    /// Args:
    ///     config: Where and how to connect.
    ///
    /// Returns:
    ///     The connected client.
    ///
    /// Raises:
    ///     Error::Server: When the server refused the credentials, the
    ///         protocol version or the session id.
    ///     Error::Timeout: When no connection succeeded in time.
    ///     Error::Connect: When the reconnect policy ran out of attempts.
    pub async fn connect(config: ClientConfig) -> Result<Client, Error> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let closed = Arc::new(OnceLock::new());
        let (ready, connected) = oneshot::channel();
        let driver = Driver::new(
            config.clone(),
            receiver,
            commands.downgrade(),
            events.clone(),
            closed.clone(),
        );
        tokio::spawn(driver.run(ready));
        let client = Client {
            commands,
            events,
            closed,
            generations: Arc::new(AtomicU64::new(1)),
            config: Arc::new(config),
        };
        match tokio::time::timeout(client.config.request_timeout, connected).await {
            Ok(Ok(Ok(()))) => Ok(client),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(client.closed_error()),
            Err(_) => {
                client.close();
                Err(Error::Timeout)
            }
        }
    }

    /// Subscribes to a topic, or with a pattern such as `orders/#` to every
    /// matching topic, including ones created later.
    ///
    /// Args:
    ///     topic: The topic or pattern.
    ///     from: Where replay starts. `StartFrom::Resume` continues a
    ///         durable session's stored subscription, and replays nothing
    ///         otherwise.
    ///
    /// Returns:
    ///     The subscription, which yields messages in `seq` order.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it, for example
    ///         `not_authorized` or `invalid_topic`.
    ///     Error::Timeout: When it didn't complete in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn subscribe(
        &self,
        topic: impl Into<String>,
        from: StartFrom,
    ) -> Result<Subscription, Error> {
        self.subscribe_as(topic.into(), from, None).await
    }

    /// Joins a shared subscription: each message on the topic goes to one
    /// member of the group, so several workers can split a topic's messages.
    ///
    /// The group keeps its own position on the server. `from` only decides
    /// where the group starts the first time it is used on the topic; later
    /// members, and this one after a reconnect, carry on from the group's
    /// position. A member that disconnects hands its unacknowledged
    /// messages to the others. Messages arrive in the order the server sends
    /// them to this member, which is not strictly `seq` order.
    ///
    /// Args:
    ///     topic: The topic, by its exact name.
    ///     group: The group's name: 1 to 64 letters, digits, `_`, `.` or `-`.
    ///     from: Where a new group starts.
    ///
    /// Returns:
    ///     The subscription, which yields this member's share.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it, for example
    ///         `not_authorized`, `invalid_topic` or `bad_request`.
    ///     Error::Timeout: When it didn't complete in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn subscribe_shared(
        &self,
        topic: impl Into<String>,
        group: impl Into<String>,
        from: StartFrom,
    ) -> Result<Subscription, Error> {
        self.subscribe_as(topic.into(), from, Some(group.into()))
            .await
    }

    /// Subscribes to a topic, alone or as a member of a group.
    ///
    /// Args:
    ///     topic: The topic or pattern.
    ///     from: Where replay starts.
    ///     group: The shared subscription to join, if any.
    ///
    /// Returns:
    ///     The subscription.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it.
    ///     Error::Timeout: When it didn't complete in time.
    ///     Error::Closed: When the client has stopped.
    async fn subscribe_as(
        &self,
        topic: String,
        from: StartFrom,
        group: Option<String>,
    ) -> Result<Subscription, Error> {
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        let (buffer, messages) = mpsc::channel(self.config.subscription_buffer.max(1));
        let (reply, outcome) = oneshot::channel();
        self.command(Command::Subscribe {
            topic: topic.clone(),
            from,
            group,
            generation,
            buffer,
            reply,
        })?;
        let subscription = Subscription {
            topic,
            messages,
            commands: self.commands.clone(),
            generation,
        };
        self.wait(outcome).await.map(|_: Subscribed| subscription)
    }

    /// Unsubscribes from a topic. A durable session forgets the stored
    /// subscription too.
    ///
    /// Args:
    ///     topic: The topic.
    ///
    /// Raises:
    ///     Error::Server: When the session store couldn't be updated.
    ///     Error::Timeout: When it didn't complete in time.
    ///     Error::Closed: When the client has stopped.
    pub async fn unsubscribe(&self, topic: impl Into<String>) -> Result<(), Error> {
        let (reply, outcome) = oneshot::channel();
        self.command(Command::Unsubscribe {
            topic: topic.into(),
            reply,
        })?;
        self.wait(outcome).await
    }

    /// Publishes a message at QoS 0: sent once, with no confirmation.
    ///
    /// While disconnected, up to 10,000 such publishes are held and sent
    /// after reconnecting. Failures the server reports arrive as
    /// `Event::ServerError`.
    ///
    /// Args:
    ///     topic: The topic.
    ///     payload: The body.
    ///
    /// Raises:
    ///     Error::Closed: When the client has stopped.
    pub fn publish(
        &self,
        topic: impl Into<String>,
        payload: impl Into<String>,
    ) -> Result<(), Error> {
        self.command(Command::Publish {
            topic: topic.into(),
            payload: payload.into(),
            encoding: None,
            confirm: None,
        })
    }

    /// Publishes bytes at QoS 0, for a body that isn't text. They travel as
    /// base64, so they take about a third more room against the server's
    /// payload limit; subscribers read them back with `Message::bytes`.
    ///
    /// Otherwise it behaves like `publish`.
    ///
    /// Args:
    ///     topic: The topic.
    ///     bytes: The body.
    ///
    /// Raises:
    ///     Error::Closed: When the client has stopped.
    pub fn publish_bytes(&self, topic: impl Into<String>, bytes: &[u8]) -> Result<(), Error> {
        self.command(Command::Publish {
            topic: topic.into(),
            payload: STANDARD.encode(bytes),
            encoding: Some(Encoding::Base64),
            confirm: None,
        })
    }

    /// Publishes a message at QoS 1 and waits until the server has stored it.
    ///
    /// The message gets a new id, and is resent under that id after a
    /// reconnect until it is confirmed, so the server stores it once.
    ///
    /// Args:
    ///     topic: The topic.
    ///     payload: The body.
    ///
    /// Returns:
    ///     Its `seq` and id.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it, for example
    ///         `not_authorized`, `rate_limited` or `payload_too_large`.
    ///     Error::Timeout: When it wasn't confirmed in time. It may still
    ///         have been stored.
    ///     Error::Closed: When the client has stopped.
    pub async fn publish_confirmed(
        &self,
        topic: impl Into<String>,
        payload: impl Into<String>,
    ) -> Result<Published, Error> {
        let (reply, outcome) = oneshot::channel();
        self.command(Command::Publish {
            topic: topic.into(),
            payload: payload.into(),
            encoding: None,
            confirm: Some((uuid::Uuid::new_v4().to_string(), reply)),
        })?;
        self.wait(outcome).await
    }

    /// Publishes bytes at QoS 1 and waits until the server has stored them,
    /// for a body that isn't text. They travel as base64, so they take about
    /// a third more room against the server's payload limit; subscribers
    /// read them back with `Message::bytes`.
    ///
    /// Otherwise it behaves like `publish_confirmed`.
    ///
    /// Args:
    ///     topic: The topic.
    ///     bytes: The body.
    ///
    /// Returns:
    ///     Its `seq` and id.
    ///
    /// Raises:
    ///     Error::Server: When the server refused it, for example
    ///         `not_authorized`, `rate_limited` or `payload_too_large`.
    ///     Error::Timeout: When it wasn't confirmed in time. It may still
    ///         have been stored.
    ///     Error::Closed: When the client has stopped.
    pub async fn publish_bytes_confirmed(
        &self,
        topic: impl Into<String>,
        bytes: &[u8],
    ) -> Result<Published, Error> {
        let (reply, outcome) = oneshot::channel();
        self.command(Command::Publish {
            topic: topic.into(),
            payload: STANDARD.encode(bytes),
            encoding: Some(Encoding::Base64),
            confirm: Some((uuid::Uuid::new_v4().to_string(), reply)),
        })?;
        self.wait(outcome).await
    }

    /// The admin requests: creating accounts, granting and revoking topics,
    /// disabling and deleting accounts. They need the client to be logged
    /// in as an admin.
    ///
    /// Returns:
    ///     A handle that sends them through this client.
    pub fn admin(&self) -> Admin<'_> {
        Admin::new(self)
    }

    /// Sends a request and waits for its reply, without queueing it while
    /// disconnected or resending it after a reconnect.
    ///
    /// Args:
    ///     message: The request.
    ///
    /// Returns:
    ///     The reply.
    ///
    /// Raises:
    ///     Error::Server: When the reply is an `error` frame.
    ///     Error::Connect: When disconnected, or the connection dropped first.
    ///     Error::Timeout: When no reply arrived in time.
    ///     Error::Closed: When the client has stopped.
    pub(crate) async fn request(&self, message: ClientMessage) -> Result<ServerMessage, Error> {
        let (reply, outcome) = oneshot::channel();
        self.command(Command::Request { message, reply })?;
        self.wait(outcome).await
    }

    /// Subscribes to the client's events: connects, disconnects,
    /// truncations, stalls and token refreshes.
    ///
    /// Returns:
    ///     A receiver of every event from now on.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Closes the connection and stops the client. Pending requests fail
    /// with `Error::Closed`.
    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
    }

    /// Why the client stopped, if it has.
    ///
    /// Returns:
    ///     `Some(code)` once stopped (the code is `None` when it was closed
    ///     by the application or ran out of attempts), `None` while running.
    pub fn stopped(&self) -> Option<Option<CloseCode>> {
        self.closed.get().copied()
    }

    /// Queues a command for the driver.
    ///
    /// Args:
    ///     command: The command.
    ///
    /// Raises:
    ///     Error::Closed: When the driver has stopped.
    fn command(&self, command: Command) -> Result<(), Error> {
        self.commands.send(command).map_err(|_| self.closed_error())
    }

    /// Waits for a request's outcome, up to the request timeout.
    ///
    /// Args:
    ///     outcome: Where the driver sends it.
    ///
    /// Returns:
    ///     The outcome.
    ///
    /// Raises:
    ///     Error::Timeout: When it didn't arrive in time.
    ///     Error::Closed: When the driver stopped first.
    async fn wait<T>(&self, outcome: oneshot::Receiver<Result<T, Error>>) -> Result<T, Error> {
        match tokio::time::timeout(self.config.request_timeout, outcome).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(self.closed_error()),
            Err(_) => Err(Error::Timeout),
        }
    }

    /// The error for calls after the client stopped.
    ///
    /// Returns:
    ///     `Error::Closed` with the close code that stopped it.
    fn closed_error(&self) -> Error {
        Error::Closed(self.closed.get().copied().flatten())
    }
}
