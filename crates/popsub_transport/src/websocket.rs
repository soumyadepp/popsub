//! WebSocket server that turns JSON protocol frames into broker operations.
//!
//! `Server` holds what every connection shares: the broker, the auth service
//! and the rate limiters. Each accepted connection gets a `Session`, which
//! owns that connection's id, remote address and outgoing channel, and has
//! one method per client message type. Only `register`, `login` and `auth`
//! are accepted before the connection presents a valid JWT; anything else
//! sent first, or a failed `auth`, closes the connection. Subscribe and
//! publish are checked against the user's topic permissions, and login,
//! registration and publishing are rate limited.
//!
//! Replies go straight onto the session's own outgoing channel, so sending
//! one never takes the broker lock. The broker is only reached through
//! `SharedBroker::with`, whose closures can't await. The auth service needs
//! no lock of its own: its methods take `&self` and its user store does its
//! own locking.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use popsub_auth::{AuthConfig, AuthError, AuthService, DefaultUserRole};
use popsub_broker::SharedBroker;
use popsub_broker::message::Message;
use popsub_client::Client;
use popsub_config::ServerSettings;
use popsub_ratelimit::{LoginRateLimiter, LoginStatus, RateLimiter, SlidingWindow, TokenBucket};
use popsub_utils::error::{PopSubError, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tracing::{error, info, warn};
use tungstenite::protocol::Message as WsMessage;

use crate::message::{ClientMessage, ServerMessage};

/// How long the accept loop waits after a failed `accept` before trying again,
/// so a persistent error such as running out of file descriptors doesn't spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The rate limiters one WebSocket server shares across all its connections.
///
/// Each limiter is behind an `Arc`, so cloning this struct shares the same
/// counters rather than copying them.
#[derive(Clone)]
pub struct RateLimiters {
    /// Failed logins per address and username pair. Keying by the pair means
    /// someone guessing passwords for a user locks out only themselves, not
    /// the real user connecting from elsewhere.
    pub login_limiter: Arc<LoginRateLimiter>,
    /// Failed logins per address across all usernames, to slow down guessing
    /// spread over many accounts.
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    /// Registration attempts per address. Every attempt counts, so
    /// reconnecting doesn't reset the allowance.
    pub register_limiter: Arc<SlidingWindow>,
    /// Published messages per authenticated user, shared by all of that
    /// user's connections.
    pub message_limiter: Arc<TokenBucket>,
}

/// How often the server's background task prunes idle rate limiter entries.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);
/// How long a login limiter key may be idle before it is forgotten. It matches
/// the longest lockout, so pruning never ends a lockout early.
const LOGIN_IDLE: Duration = Duration::from_secs(3600);
/// How long a message bucket may be idle before it is forgotten. A bucket
/// refills fully well within this time, so forgetting it changes nothing.
const MESSAGE_IDLE: Duration = Duration::from_secs(600);

impl RateLimiters {
    /// Removes entries for keys that have been quiet long enough to forget.
    ///
    /// Login entries are kept for `LOGIN_IDLE` and message buckets for
    /// `MESSAGE_IDLE`. The registration window drops entries whose attempts
    /// have all aged out of its own window.
    ///
    /// Returns:
    ///     The total number of entries removed across all four limiters.
    pub fn prune_idle(&self) -> usize {
        self.login_limiter.prune_idle(LOGIN_IDLE)
            + self.ip_login_limiter.prune_idle(LOGIN_IDLE)
            + self.register_limiter.prune_idle()
            + self.message_limiter.prune_idle(MESSAGE_IDLE)
    }
}

impl Default for RateLimiters {
    /// Builds the limits the server uses unless it is given others.
    ///
    /// An address and username pair is locked out after 5 failed logins for
    /// 5 minutes, and an address is locked out after 20 failed logins across
    /// any usernames for 15 minutes. Both lockouts back off exponentially up to
    /// 1 hour. An address may make 5 registration attempts per hour, and a user
    /// may publish 100 messages per second with a burst of 50.
    ///
    /// Returns:
    ///     The default set of limiters.
    fn default() -> Self {
        Self {
            login_limiter: Arc::new(
                LoginRateLimiter::builder()
                    .max_attempts(5)
                    .lockout_duration(Duration::from_secs(300))
                    .exponential_backoff(true)
                    .max_lockout_duration(Duration::from_secs(3600))
                    .build(),
            ),
            ip_login_limiter: Arc::new(
                LoginRateLimiter::builder()
                    .max_attempts(20)
                    .lockout_duration(Duration::from_secs(900))
                    .exponential_backoff(true)
                    .max_lockout_duration(Duration::from_secs(3600))
                    .build(),
            ),
            register_limiter: Arc::new(SlidingWindow::new(5, Duration::from_secs(3600))),
            message_limiter: Arc::new(TokenBucket::new(100, 50, Duration::from_secs(1))),
        }
    }
}

/// The WebSocket server: shared state plus the accept loop.
///
/// Build one with `Server::new` or `Server::from_settings`, optionally replace
/// the rate limits with `with_rate_limiters`, then call `listen` or `serve`.
/// Cloning is cheap and every clone shares the same broker, users and limits.
#[derive(Clone)]
pub struct Server {
    /// The broker every connection publishes and subscribes through.
    broker: SharedBroker,
    /// Users, passwords, tokens and topic permissions.
    auth: Arc<AuthService>,
    /// Limits shared by every connection on this server.
    limiters: RateLimiters,
}

impl Server {
    /// Creates a server with the default rate limits.
    ///
    /// Args:
    ///     broker: The broker every connection shares.
    ///     auth: The service that handles register, login and token checks
    ///         and decides which topics each user may use.
    ///
    /// Returns:
    ///     A server ready to `listen` or `serve`.
    pub fn new(broker: SharedBroker, auth: Arc<AuthService>) -> Self {
        Self {
            broker,
            auth,
            limiters: RateLimiters::default(),
        }
    }

    /// Creates a server whose auth service is configured from the `[server]` settings.
    ///
    /// The JWT secret and expiry, the admin user, whether self-registration
    /// is allowed and the role new users get (see `registration_role`) all
    /// come from `settings`. Rate limits are the defaults.
    ///
    /// Args:
    ///     broker: The broker every connection shares.
    ///     settings: The `[server]` section of the loaded configuration.
    ///
    /// Returns:
    ///     A server ready to `listen` or `serve`.
    pub async fn from_settings(broker: SharedBroker, settings: &ServerSettings) -> Self {
        let config = AuthConfig::new(&settings.jwt_secret)
            .with_expiration(settings.jwt_expiration_hours)
            .with_admin(&settings.username, &settings.password)
            .with_registration(settings.allow_registration)
            .with_default_role(registration_role(&settings.registration_role));
        let auth = Arc::new(AuthService::new(config).await);
        Self::new(broker, auth)
    }

    /// Replaces the default rate limits.
    ///
    /// Args:
    ///     limiters: The limiters to use for every connection.
    ///
    /// Returns:
    ///     The server with the new limits.
    pub fn with_rate_limiters(mut self, limiters: RateLimiters) -> Self {
        self.limiters = limiters;
        self
    }

    /// Binds `addr` and serves connections on it until the future is dropped.
    ///
    /// Args:
    ///     addr: The address to listen on, such as `127.0.0.1:8080`.
    ///
    /// Raises:
    ///     std::io::Error: When the address can't be bound.
    pub async fn listen(self, addr: &str) -> std::io::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        info!(
            "WebSocket server listening on ws://{}",
            listener.local_addr()?
        );
        self.serve(listener).await;
        Ok(())
    }

    /// Serves connections on an already bound listener until the future is dropped.
    ///
    /// Taking a bound listener lets tests bind port 0 and learn the real port
    /// before the server starts. A background task prunes idle rate limiter
    /// entries every `PRUNE_INTERVAL` and is stopped when this future is
    /// dropped. A failed `accept` is logged and retried after
    /// `ACCEPT_BACKOFF` instead of stopping the server. Each connection runs
    /// in its own task.
    ///
    /// Args:
    ///     listener: The bound listener to accept connections from.
    pub async fn serve(self, listener: TcpListener) {
        let _pruner = AbortOnDrop(tokio::spawn(prune_forever(self.limiters.clone())));
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    tokio::spawn(self.clone().run_connection(stream, peer));
                }
                Err(e) => {
                    warn!(%e, "accepting a connection failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            }
        }
    }

    /// Runs one connection from handshake to cleanup.
    ///
    /// After the WebSocket handshake a `Client` is registered with the broker
    /// and a send loop starts draining the client's outgoing channel. Text
    /// frames are parsed and passed to the session; invalid JSON is logged
    /// and skipped, and binary frames are ignored. The connection ends when
    /// the socket closes, a handler asks to close it, or a handler fails, in
    /// which case the client is sent an `internal server error` frame first.
    /// The client is removed from the broker once, by whichever of this loop
    /// and the send loop finishes first.
    ///
    /// Args:
    ///     stream: The accepted TCP connection.
    ///     peer: The connection's remote address, used for rate limit keys.
    async fn run_connection(self, stream: TcpStream, peer: SocketAddr) {
        let ws_stream = match accept_async(stream).await {
            Ok(ws) => ws,
            Err(e) => {
                warn!(%peer, %e, "WebSocket handshake failed");
                return;
            }
        };
        let (ws_sender, mut ws_receiver) = ws_stream.split();
        let (outbox, rx) = mpsc::unbounded_channel::<WsMessage>();
        let client = Client::new(outbox.clone());
        let client_id = client.id.clone();
        self.broker.with(|broker| broker.register_client(client));

        let cleanup = ClientCleanup::new(self.broker.clone(), client_id.clone());
        spawn_send_loop(ws_sender, rx, client_id.clone(), cleanup.clone());

        let session = Session {
            server: self,
            client_id,
            peer_ip: peer.ip().to_string(),
            outbox,
        };

        while let Some(Ok(frame)) = ws_receiver.next().await {
            let text = match frame {
                WsMessage::Text(text) => text,
                WsMessage::Close(_) => break,
                WsMessage::Binary(_) => {
                    warn!(client_id = %session.client_id, "ignoring binary frame");
                    continue;
                }
                _ => continue,
            };
            let parsed = match serde_json::from_str::<ClientMessage>(&text) {
                Ok(parsed) => parsed,
                Err(err) => {
                    warn!(
                        client_id = %session.client_id,
                        "invalid client message: {} | {}",
                        err,
                        text.chars().take(100).collect::<String>()
                    );
                    continue;
                }
            };
            match session.handle(parsed).await {
                Ok(Flow::Continue) => {}
                Ok(Flow::Close) => break,
                Err(e) => {
                    error!(client_id = %session.client_id, "error handling message: {}", e);
                    let _ = session.reply(ServerMessage::Error {
                        message: "internal server error".to_string(),
                    });
                    break;
                }
            }
        }

        cleanup.run();
    }
}

/// Prunes idle rate limiter entries every `PRUNE_INTERVAL`, forever.
///
/// Args:
///     limiters: The limiters to prune.
async fn prune_forever(limiters: RateLimiters) {
    let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
    loop {
        ticker.tick().await;
        let pruned = limiters.prune_idle();
        if pruned > 0 {
            info!(pruned, "pruned idle rate limiter entries");
        }
    }
}

/// Aborts a spawned task when dropped, tying the task's life to its owner.
struct AbortOnDrop(
    /// The task to abort.
    JoinHandle<()>,
);

impl Drop for AbortOnDrop {
    /// Aborts the task.
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Whether a connection should keep reading after a handler returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep the connection open.
    Continue,
    /// Close the connection.
    Close,
}

/// Removes a client from the broker exactly once.
///
/// The reader loop and the send loop each hold a clone, and whichever ends
/// first does the removal.
#[derive(Clone)]
struct ClientCleanup {
    /// The broker to remove the client from.
    broker: SharedBroker,
    /// The client to remove.
    client_id: String,
    /// Set by the first call to `run`.
    done: Arc<AtomicBool>,
}

impl ClientCleanup {
    /// Prepares the removal of one client.
    ///
    /// Args:
    ///     broker: The broker the client is registered with.
    ///     client_id: The client to remove.
    ///
    /// Returns:
    ///     A cleanup that has not run yet.
    fn new(broker: SharedBroker, client_id: String) -> Self {
        Self {
            broker,
            client_id,
            done: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Removes the client and its subscriptions, unless a clone already did.
    fn run(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            self.broker
                .with(|broker| broker.cleanup_client(&self.client_id));
        }
    }
}

/// Spawns a task that forwards frames from a client's outgoing channel to its WebSocket.
///
/// The task stops when every sender for the channel has been dropped or a
/// write to the socket fails, then runs `cleanup`.
///
/// Args:
///     ws_sender: The write half of the client's WebSocket.
///     rx: The receiving end of the channel the broker and session send to.
///     client_id: The client's broker id, for logging.
///     cleanup: Removes the client from the broker, once.
fn spawn_send_loop<S>(
    mut ws_sender: S,
    mut rx: mpsc::UnboundedReceiver<WsMessage>,
    client_id: String,
    cleanup: ClientCleanup,
) where
    S: futures_util::Sink<WsMessage, Error = tungstenite::Error> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = ws_sender.send(msg).await {
                warn!(client_id = %client_id, %e, "failed to write to socket");
                break;
            }
        }
        cleanup.run();
        info!(client_id = %client_id, "send loop closed");
    });
}

/// One connection's view of the server, with a handler per client message type.
struct Session {
    /// The state every connection shares.
    server: Server,
    /// This connection's id in the broker.
    client_id: String,
    /// This connection's remote address, the key for login and registration limits.
    peer_ip: String,
    /// This connection's outgoing channel. Replies are queued here directly.
    outbox: mpsc::UnboundedSender<WsMessage>,
}

impl Session {
    /// Dispatches a parsed client frame to its handler.
    ///
    /// Args:
    ///     msg: The frame the client sent.
    ///
    /// Returns:
    ///     Whether to keep the connection open.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When a reply can't be queued because the
    ///         connection's outgoing channel is closed.
    async fn handle(&self, msg: ClientMessage) -> Result<Flow> {
        match msg {
            ClientMessage::Register { username, password } => {
                self.register(&username, &password).await
            }
            ClientMessage::Login { username, password } => self.login(&username, &password).await,
            ClientMessage::Auth { token } => self.authenticate(&token).await,
            ClientMessage::Subscribe { topic } => self.subscribe(&topic).await,
            ClientMessage::Unsubscribe { topic } => self.unsubscribe(&topic),
            ClientMessage::Publish {
                topic,
                payload,
                message_id,
                qos,
            } => self.publish(&topic, payload, message_id, qos).await,
            ClientMessage::Ack { message_id } => self.ack(&message_id),
        }
    }

    /// Queues a frame on this connection's outgoing channel.
    ///
    /// Args:
    ///     message: The frame to send.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn reply(&self, message: ServerMessage) -> Result<()> {
        let json = serde_json::to_string(&message)?;
        self.outbox
            .send(WsMessage::Text(json.into()))
            .map_err(|_| PopSubError::Client("outgoing channel closed".to_string()))
    }

    /// Queues an `error` frame with the given text.
    ///
    /// Args:
    ///     message: The error text for the client.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn reply_error(&self, message: impl Into<String>) -> Result<()> {
        self.reply(ServerMessage::Error {
            message: message.into(),
        })
    }

    /// Returns the authenticated user, or tells the client to authenticate first.
    ///
    /// Every handler except `register`, `login` and `auth` starts here. A
    /// client that is no longer registered, or hasn't authenticated, gets no
    /// user back and its connection should be closed; the second also gets a
    /// `must authenticate first` error.
    ///
    /// Args:
    ///     action: What the client tried to do, for the log.
    ///
    /// Returns:
    ///     The username, or `None` when the connection should close.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the error frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn require_user(&self, action: &str) -> Result<Option<String>> {
        let state = self.server.broker.with(|broker| {
            broker
                .clients
                .get(&self.client_id)
                .map(|client| (client.authenticated, client.username.clone()))
        });
        match state {
            None => {
                warn!(client_id = %self.client_id, action, "client not registered");
                Ok(None)
            }
            Some((false, _)) => {
                warn!(client_id = %self.client_id, action, "request before authentication");
                self.reply_error("must authenticate first")?;
                Ok(None)
            }
            Some((true, username)) => Ok(Some(username.unwrap_or_default())),
        }
    }

    /// Creates a user account and replies with a `register_response`.
    ///
    /// Every attempt counts against the registration limit for this address,
    /// whether or not it succeeds. Over the limit, the reply says how many
    /// seconds to wait and the auth service isn't called. A failed
    /// registration is reported with `registration_error_message`, which
    /// doesn't reveal whether the username is taken. The connection stays open.
    ///
    /// Args:
    ///     username: The requested username.
    ///     password: The requested password.
    ///
    /// Returns:
    ///     Always `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn register(&self, username: &str, password: &str) -> Result<Flow> {
        let limiter = &self.server.limiters.register_limiter;
        if !limiter.try_acquire(&self.peer_ip) {
            let retry_after = limiter.retry_after(&self.peer_ip).unwrap_or(1);
            warn!(client_id = %self.client_id, peer_ip = %self.peer_ip, retry_after, "registration rate limited");
            self.reply(ServerMessage::RegisterResponse {
                success: false,
                message: format!("Too many attempts. Try again in {retry_after} seconds."),
            })?;
            return Ok(Flow::Continue);
        }

        let result = self.server.auth.register_user(username, password).await;
        let response = match result {
            Ok(()) => {
                info!(client_id = %self.client_id, username, "registration successful");
                ServerMessage::RegisterResponse {
                    success: true,
                    message: "User registered successfully".to_string(),
                }
            }
            Err(e) => {
                warn!(client_id = %self.client_id, username, "registration failed: {}", e);
                ServerMessage::RegisterResponse {
                    success: false,
                    message: registration_error_message(&e),
                }
            }
        };
        self.reply(response)?;
        Ok(Flow::Continue)
    }

    /// Checks a username and password and replies with a JWT or an error.
    ///
    /// The attempt is refused with `Too many login attempts`, without calling
    /// the auth service, while this address or this address and username pair
    /// is locked out. Only wrong credentials count as failures, against both
    /// limiters; a success clears the pair's failures but not the address
    /// count, so one valid account can't be used to reset guessing against
    /// others. Every login error is reported as `invalid credentials`. The
    /// connection stays open.
    ///
    /// Args:
    ///     username: The username to log in as.
    ///     password: The password to check.
    ///
    /// Returns:
    ///     Always `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn login(&self, username: &str, password: &str) -> Result<Flow> {
        let limiters = &self.server.limiters;
        let pair_key = format!("{}|{username}", self.peer_ip);
        let lockout = match limiters.ip_login_limiter.check_status(&self.peer_ip) {
            LoginStatus::LockedOut { retry_after } => Some(retry_after),
            _ => match limiters.login_limiter.check_status(&pair_key) {
                LoginStatus::LockedOut { retry_after } => Some(retry_after),
                _ => None,
            },
        };
        if let Some(retry_after) = lockout {
            warn!(client_id = %self.client_id, peer_ip = %self.peer_ip, username, retry_after, "login rate limited");
            self.reply_error(format!(
                "Too many login attempts. Try again in {retry_after} seconds."
            ))?;
            return Ok(Flow::Continue);
        }

        let result = self.server.auth.login(username, password).await;
        match result {
            Ok(token) => {
                info!(client_id = %self.client_id, username, "login successful");
                limiters.login_limiter.record_success(&pair_key);
                self.reply(ServerMessage::LoginResponse { token })?;
            }
            Err(e) => {
                warn!(client_id = %self.client_id, username, "login failed: {}", e);
                if matches!(e, AuthError::InvalidCredentials) {
                    limiters.login_limiter.record_failure(&pair_key);
                    limiters.ip_login_limiter.record_failure(&self.peer_ip);
                }
                self.reply_error("invalid credentials")?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Validates a JWT and marks the connection as authenticated as its subject.
    ///
    /// A valid token sets the client's `authenticated` flag and username and
    /// gets an `authenticated` reply. An invalid or expired token gets an
    /// `authentication failed` error and closes the connection, as does a
    /// client that is no longer registered.
    ///
    /// Args:
    ///     token: The JWT from a previous `login`.
    ///
    /// Returns:
    ///     `Flow::Continue` when the token was valid, otherwise `Flow::Close`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn authenticate(&self, token: &str) -> Result<Flow> {
        let validated = self.server.auth.validate_token(token);
        let claims = match validated {
            Ok(claims) => claims,
            Err(e) => {
                warn!(client_id = %self.client_id, "authentication failed: {}", e);
                self.reply_error("authentication failed")?;
                return Ok(Flow::Close);
            }
        };
        let registered = self.server.broker.with(|broker| {
            broker
                .clients
                .get_mut(&self.client_id)
                .map(|client| {
                    client.authenticated = true;
                    client.username = Some(claims.sub.clone());
                })
                .is_some()
        });
        if !registered {
            warn!(client_id = %self.client_id, "client not registered");
            return Ok(Flow::Close);
        }
        info!(client_id = %self.client_id, username = %claims.sub, "authenticated successfully");
        self.reply(ServerMessage::Authenticated {})?;
        Ok(Flow::Continue)
    }

    /// Subscribes the connection to a topic if its user may read it.
    ///
    /// A user without permission gets a `not authorized` error and stays
    /// connected. Subscribing replays the topic's stored messages to the
    /// client.
    ///
    /// Args:
    ///     topic: The topic to subscribe to.
    ///
    /// Returns:
    ///     `Flow::Close` when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn subscribe(&self, topic: &str) -> Result<Flow> {
        let Some(username) = self.require_user("subscribe")? else {
            return Ok(Flow::Close);
        };
        let allowed = self.server.auth.can_subscribe(&username, topic).await;
        if !allowed {
            warn!(client_id = %self.client_id, username, topic, "subscribe denied");
            self.reply_error(format!("not authorized to subscribe to '{topic}'"))?;
            return Ok(Flow::Continue);
        }
        self.server
            .broker
            .with(|broker| broker.subscribe(topic, self.client_id.clone()));
        info!(client_id = %self.client_id, username, topic, "subscribed");
        Ok(Flow::Continue)
    }

    /// Unsubscribes the connection from a topic.
    ///
    /// No permission check is made, since leaving a topic grants nothing.
    /// Leaving a topic the client isn't on does nothing.
    ///
    /// Args:
    ///     topic: The topic to leave.
    ///
    /// Returns:
    ///     `Flow::Close` when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn unsubscribe(&self, topic: &str) -> Result<Flow> {
        let Some(username) = self.require_user("unsubscribe")? else {
            return Ok(Flow::Close);
        };
        self.server
            .broker
            .with(|broker| broker.unsubscribe(topic, &self.client_id));
        info!(client_id = %self.client_id, username, topic, "unsubscribed");
        Ok(Flow::Continue)
    }

    /// Publishes a payload to a topic if the user may write to it and is within its rate limit.
    ///
    /// The topic permission is checked before the rate limit, so a denied
    /// publish doesn't use up the user's allowance. Both a denied and a rate
    /// limited publish get an error and keep the connection open. The broker
    /// sets the timestamp and generates an id when `message_id` is missing.
    ///
    /// Args:
    ///     topic: The topic to publish to.
    ///     payload: The message body.
    ///     message_id: The client's id for the message, used to match its `ack`.
    ///     qos: The delivery level, 0 or 1. Defaults to 0.
    ///
    /// Returns:
    ///     `Flow::Close` when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn publish(
        &self,
        topic: &str,
        payload: String,
        message_id: Option<String>,
        qos: Option<u8>,
    ) -> Result<Flow> {
        let Some(username) = self.require_user("publish")? else {
            return Ok(Flow::Close);
        };
        let allowed = self.server.auth.can_publish(&username, topic).await;
        if !allowed {
            warn!(client_id = %self.client_id, username, topic, "publish denied");
            self.reply_error(format!("not authorized to publish to '{topic}'"))?;
            return Ok(Flow::Continue);
        }
        if !self.server.limiters.message_limiter.try_acquire(&username) {
            warn!(client_id = %self.client_id, username, "message rate limited");
            self.reply_error("Rate limited. Too many messages.")?;
            return Ok(Flow::Continue);
        }
        let message = Message {
            topic: topic.to_string(),
            payload,
            timestamp: 0,
            message_id: message_id.unwrap_or_default(),
            qos: qos.unwrap_or(0),
        };
        self.server.broker.with(|broker| broker.publish(message));
        info!(client_id = %self.client_id, username, topic, "published");
        Ok(Flow::Continue)
    }

    /// Acknowledges a QoS 1 message so the broker stops retrying it.
    ///
    /// The ack is matched by message id alone, without checking that the
    /// message was delivered to this client (see `docs/DESIGN.md` section 12).
    ///
    /// Args:
    ///     message_id: The id of the message being acknowledged.
    ///
    /// Returns:
    ///     `Flow::Close` when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn ack(&self, message_id: &str) -> Result<Flow> {
        if self.require_user("ack")?.is_none() {
            return Ok(Flow::Close);
        }
        self.server
            .broker
            .with(|broker| broker.handle_ack(message_id));
        Ok(Flow::Continue)
    }
}

/// Turns a registration error into text that is safe to show the client.
///
/// Validation messages are shown as they are, because they help a genuine
/// user fix their input. Whether a username is already taken is not
/// revealed, so the endpoint can't be used to list existing accounts.
///
/// Args:
///     error: The error from the auth service.
///
/// Returns:
///     The reason from a `PermissionDenied` error, or `registration failed`
///     for any other error.
fn registration_error_message(error: &AuthError) -> String {
    match error {
        AuthError::PermissionDenied(reason) => reason.clone(),
        _ => "registration failed".to_string(),
    }
}

/// Maps the `server.registration_role` setting to the role self-registered
/// users receive.
///
/// Args:
///     name: `none`, `readonly` or `full`, matched case sensitively. Anything
///         else is treated as `none` and logged, so a typo never grants more
///         access than intended.
///
/// Returns:
///     The default role for new users: full access, read only, or limited
///     access to no topics.
pub fn registration_role(name: &str) -> DefaultUserRole {
    match name {
        "full" => DefaultUserRole::FullAccess,
        "readonly" => DefaultUserRole::ReadOnly,
        "none" => DefaultUserRole::LimitedAccess(Vec::new()),
        other => {
            warn!(role = %other, "unknown registration_role, giving new users no topics");
            DefaultUserRole::LimitedAccess(Vec::new())
        }
    }
}

/// Unit tests for the private helpers in this file: registration error text,
/// registration role parsing and rate limiter pruning.
#[cfg(test)]
mod limiter_tests {
    use super::*;

    /// Checks that a taken username gives a generic error while validation
    /// reasons are passed through.
    #[test]
    fn taken_usernames_are_not_revealed() {
        let taken = AuthError::UserAlreadyExists("alice".to_string());
        assert_eq!(registration_error_message(&taken), "registration failed");

        let invalid = AuthError::PermissionDenied("username must be 3 to 32 characters".into());
        assert_eq!(
            registration_error_message(&invalid),
            "username must be 3 to 32 characters"
        );
    }

    /// Checks that only the exact names `full` and `readonly` grant topics and
    /// every other name maps to no topics.
    #[test]
    fn unknown_registration_roles_grant_nothing() {
        use popsub_auth::DefaultUserRole;
        assert!(matches!(
            registration_role("full"),
            DefaultUserRole::FullAccess
        ));
        assert!(matches!(
            registration_role("readonly"),
            DefaultUserRole::ReadOnly
        ));
        for name in ["none", "admin", "Full", ""] {
            assert!(
                matches!(registration_role(name), DefaultUserRole::LimitedAccess(ref topics) if topics.is_empty()),
                "{name}"
            );
        }
    }

    /// Checks that a fresh message bucket survives the default prune but is
    /// dropped once its idle limit is zero.
    #[test]
    fn pruning_forgets_idle_message_buckets() {
        let limiters = RateLimiters::default();
        limiters.message_limiter.try_acquire("alice");
        assert_eq!(limiters.message_limiter.tracked_keys(), 1);
        assert_eq!(limiters.prune_idle(), 0);
        assert_eq!(limiters.message_limiter.prune_idle(Duration::ZERO), 1);
    }
}
