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
//! Each connection's loop also pings the client every heartbeat interval and
//! closes it after the idle timeout with nothing received, and closes it when
//! its JWT expires unless the client sends `auth` again with a new token
//! (it is warned with `auth_expiring` a minute before). Every close the
//! server starts carries a `CloseCode`, so a client SDK knows whether to
//! reconnect.
//!
//! Replies go straight onto the session's own outgoing channel, so sending
//! one never takes the broker lock. Subscribe and publish are `async` calls
//! on `SharedBroker`, which hands them to the topic's writer; everything else
//! reaches the broker through `SharedBroker::with`, whose closures can't
//! await. The auth service needs no lock of its own: its methods take
//! `&self` and its user store does its own locking.

mod admin;

use popsub_utils::pii::pii;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use popsub_auth::{
    AuthConfig, AuthError, AuthService, DefaultUserRole, InMemoryUserStore, UserStore,
};
use popsub_broker::{SharedBroker, close_message, session_key};
use popsub_config::{LimitSettings, ServerSettings};
use popsub_connection::{Client, Outbox, OutboxReceiver, Outgoing, QueueLimits};
use popsub_ratelimit::{LoginRateLimiter, LoginStatus, RateLimiter, SlidingWindow, TokenBucket};
use popsub_storage::{MAX_TOPIC_BYTES, NewRecord};
use popsub_utils::error::{PopSubError, Result};
use popsub_utils::metrics::{attr, metrics};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::accept_hdr_async_with_config;
use tracing::{Instrument, error, info, info_span, warn};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::Message as WsMessage;
use tungstenite::protocol::WebSocketConfig;

use crate::proxy::{TrustedProxies, read_proxy_header};
use crate::tls::ServerTls;

use popsub_protocol::{
    ClientFrame, ClientMessage, CloseCode, ErrorCode, Limits, PROTOCOL_VERSION, RequestId,
    ServerFrame, ServerMessage, StartFrom,
};

/// The longest `session_id` a client may choose, in characters.
const MAX_SESSION_ID_CHARS: usize = 128;

/// The longest `message_id` a publisher may choose, in characters.
const MAX_MESSAGE_ID_CHARS: usize = 128;

/// The default longest payload a publish may carry: 1 MiB.
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Room for everything in a publish frame besides the payload (the type,
/// topic, ids and JSON quoting), added to the WebSocket frame size limit.
const FRAME_OVERHEAD_BYTES: usize = 64 * 1024;

/// How long the server waits for the client's close reply after sending a
/// close frame, before dropping the connection.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// How long a new connection has to finish the WebSocket handshake. Without
/// a limit, sockets that never send one would hold connection slots forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long before a connection's token expires the client is sent `auth_expiring`.
const AUTH_EXPIRY_WARNING: Duration = Duration::from_secs(60);

/// How often the server pings each connection and how long it waits for
/// anything back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    /// How often a WebSocket ping is sent.
    pub interval: Duration,
    /// How long a connection may send nothing, not even a pong, before it
    /// is closed with `CloseCode::IdleTimeout`.
    pub idle_timeout: Duration,
}

impl Default for Heartbeat {
    /// Builds the default heartbeat: a ping every 30 seconds, closed after 90
    /// seconds of silence.
    ///
    /// Returns:
    ///     The default heartbeat.
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(90),
        }
    }
}

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
    /// How long a login limiter key may be idle before it is forgotten. At
    /// least the longest lockout, so pruning never ends a lockout early.
    pub login_idle: Duration,
    /// How long a message bucket may be idle before it is forgotten. At
    /// least the time a bucket takes to refill, so forgetting one changes
    /// nothing.
    pub message_idle: Duration,
}

/// How often the server's background task prunes idle rate limiter entries.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);
/// The shortest time a message bucket is kept after its last use.
const MESSAGE_IDLE: Duration = Duration::from_secs(600);

impl RateLimiters {
    /// Builds the limiters from the `[limits]` settings. Values below 1 are
    /// treated as 1. Login lockouts double on each repeat, up to
    /// `max_lockout_secs` (and never shorter than the first lockout).
    ///
    /// Args:
    ///     settings: The `[limits]` section.
    ///
    /// Returns:
    ///     The limiters.
    pub fn from_settings(settings: &LimitSettings) -> Self {
        let secs = |value: u64| Duration::from_secs(value.max(1));
        let login_lockout = secs(settings.login_lockout_secs);
        let address_lockout = secs(settings.address_lockout_secs);
        let max_lockout = secs(settings.max_lockout_secs)
            .max(login_lockout)
            .max(address_lockout);
        let lockouts = |attempts: u32, lockout: Duration| {
            LoginRateLimiter::builder()
                .max_attempts(attempts.max(1))
                .lockout_duration(lockout)
                .exponential_backoff(true)
                .max_lockout_duration(max_lockout)
                .build()
        };
        let rate = settings.publish_per_second.max(1);
        let burst = settings.publish_burst.max(1);
        let refill = Duration::from_secs(burst.div_ceil(rate));
        Self {
            login_limiter: Arc::new(lockouts(settings.login_attempts, login_lockout)),
            ip_login_limiter: Arc::new(lockouts(settings.address_login_attempts, address_lockout)),
            register_limiter: Arc::new(SlidingWindow::new(
                settings.registrations_per_hour.max(1),
                Duration::from_secs(3600),
            )),
            message_limiter: Arc::new(TokenBucket::new(rate, burst, Duration::from_secs(1))),
            login_idle: max_lockout,
            message_idle: MESSAGE_IDLE.max(refill),
        }
    }

    /// Removes entries for keys that have been quiet long enough to forget.
    ///
    /// Login entries are kept for `login_idle` and message buckets for
    /// `message_idle`. The registration window drops entries whose attempts
    /// have all aged out of its own window.
    ///
    /// Returns:
    ///     The total number of entries removed across all four limiters.
    pub fn prune_idle(&self) -> usize {
        self.login_limiter.prune_idle(self.login_idle)
            + self.ip_login_limiter.prune_idle(self.login_idle)
            + self.register_limiter.prune_idle()
            + self.message_limiter.prune_idle(self.message_idle)
    }
}

impl Default for RateLimiters {
    /// Builds the limits from the default `[limits]` settings: an address
    /// and username pair is locked out after 5 failed logins for 5 minutes,
    /// and an address after 20 across any usernames for 15 minutes, both
    /// doubling up to an hour; 5 registration attempts per address per hour;
    /// and 100 publishes a second per user with bursts of 50.
    ///
    /// Returns:
    ///     The default set of limiters.
    fn default() -> Self {
        Self::from_settings(&LimitSettings::default())
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
    /// Ping interval and idle timeout for every connection.
    heartbeat: Heartbeat,
    /// The longest payload a publish may carry, in bytes.
    max_payload_bytes: usize,
    /// The byte budget of each connection's outgoing queue.
    queue_limits: QueueLimits,
    /// Proxies whose `X-Forwarded-For` header is believed.
    trusted_proxies: Arc<TrustedProxies>,
    /// The most connections at once, or `None` for no limit.
    max_connections: Option<usize>,
    /// Connections open now, counted from `accept` until the connection's
    /// task ends, shared by every clone.
    open_connections: Arc<AtomicUsize>,
    /// The most connections one client address may hold, or `None` for no limit.
    max_per_address: Option<usize>,
    /// Open connections per client address, after the handshake, shared by
    /// every clone. Addresses with none are removed.
    per_address: Arc<Mutex<HashMap<IpAddr, usize>>>,
    /// How long a new connection has to finish its TLS and WebSocket handshakes.
    handshake_timeout: Duration,
    /// The certificate to serve `wss://` with, or `None` for plain `ws://`.
    tls: Option<ServerTls>,
    /// Whether connections from trusted proxies start with a PROXY protocol header.
    proxy_protocol: bool,
}

impl Server {
    /// Creates a server with the default rate limits, the broker's
    /// connection limit and no trusted proxies.
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
            auth,
            limiters: RateLimiters::default(),
            heartbeat: Heartbeat::default(),
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            queue_limits: QueueLimits::default(),
            trusted_proxies: Arc::new(TrustedProxies::default()),
            max_connections: broker.max_connections(),
            open_connections: Arc::new(AtomicUsize::new(0)),
            max_per_address: None,
            per_address: Arc::new(Mutex::new(HashMap::new())),
            handshake_timeout: HANDSHAKE_TIMEOUT,
            tls: None,
            proxy_protocol: false,
            broker,
        }
    }

    /// Creates a server configured from the `[server]` settings, with users
    /// kept in memory (lost on restart).
    ///
    /// See `from_settings_with_users` for what comes from `settings`.
    ///
    /// Args:
    ///     broker: The broker every connection shares.
    ///     settings: The `[server]` section of the loaded configuration.
    ///
    /// Returns:
    ///     A server ready to `listen` or `serve`.
    pub async fn from_settings(broker: SharedBroker, settings: &ServerSettings) -> Self {
        Self::from_settings_with_users(broker, settings, Arc::new(InMemoryUserStore::new())).await
    }

    /// Creates a server configured from the `[server]` settings, with users
    /// kept in the given store.
    ///
    /// The JWT secret and expiry, the admin user, whether self-registration
    /// is allowed, the role new users get (see `registration_role`), the
    /// heartbeat, the payload limit, the outbound queue budget, the
    /// per-address connection limit and whether to read PROXY protocol
    /// headers all come from `settings`. Rate limits are the defaults; pass the `[limits]`
    /// section through `RateLimiters::from_settings` and `with_rate_limiters`.
    /// `server.trusted_proxies` isn't applied here, because an invalid entry
    /// must stop startup: parse it with `TrustedProxies::parse` and pass it to
    /// `with_trusted_proxies`. The admin from
    /// `settings` is added to the store only if no account with that name
    /// exists yet, so once it does, its password is changed through the
    /// store, not the settings.
    ///
    /// Args:
    ///     broker: The broker every connection shares.
    ///     settings: The `[server]` section of the loaded configuration.
    ///     users: Where accounts live, such as a `FileUserStore`.
    ///
    /// Returns:
    ///     A server ready to `listen` or `serve`.
    pub async fn from_settings_with_users(
        broker: SharedBroker,
        settings: &ServerSettings,
        users: Arc<dyn UserStore>,
    ) -> Self {
        let config = AuthConfig::new(&settings.jwt_secret)
            .with_expiration(settings.jwt_expiration_hours)
            .with_admin(&settings.username, &settings.password)
            .with_registration(settings.allow_registration)
            .with_default_role(registration_role(&settings.registration_role));
        let auth = Arc::new(AuthService::with_store(config, users).await);
        Self::new(broker, auth)
            .with_heartbeat(Heartbeat {
                interval: Duration::from_secs(settings.heartbeat_secs.max(1)),
                idle_timeout: Duration::from_secs(settings.idle_timeout_secs.max(1)),
            })
            .with_max_payload_bytes(settings.max_payload_bytes)
            .with_queue_limits(QueueLimits::from_hard_limit(settings.outbound_queue_bytes))
            .with_max_connections_per_address(
                Some(settings.max_connections_per_address).filter(|max| *max > 0),
            )
            .with_proxy_protocol(settings.proxy_protocol)
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

    /// Sets the proxies whose `X-Forwarded-For` header is believed, so rate
    /// limits key on the client behind them.
    ///
    /// Args:
    ///     proxies: The trusted proxies.
    ///
    /// Returns:
    ///     The server with them trusted.
    pub fn with_trusted_proxies(mut self, proxies: TrustedProxies) -> Self {
        self.trusted_proxies = Arc::new(proxies);
        self
    }

    /// Replaces the connection limit taken from the broker settings.
    ///
    /// Args:
    ///     max_connections: The most connections at once, or `None` for no limit.
    ///
    /// Returns:
    ///     The server with the new limit.
    pub fn with_max_connections(mut self, max_connections: Option<usize>) -> Self {
        self.max_connections = max_connections;
        self
    }

    /// Limits how many connections one client address may hold at once.
    ///
    /// Args:
    ///     max: The most per address, or `None` for no limit.
    ///
    /// Returns:
    ///     The server with the new limit.
    pub fn with_max_connections_per_address(mut self, max: Option<usize>) -> Self {
        self.max_per_address = max;
        self
    }

    /// Reads a PROXY protocol header at the start of every connection from a
    /// trusted proxy, before TLS and the WebSocket handshake. Connections
    /// from other peers are served without one.
    ///
    /// Args:
    ///     enabled: Whether to read the header.
    ///
    /// Returns:
    ///     The server with the setting applied.
    pub fn with_proxy_protocol(mut self, enabled: bool) -> Self {
        self.proxy_protocol = enabled;
        self
    }

    /// Serves `wss://`: every connection must complete a TLS handshake first.
    ///
    /// Args:
    ///     tls: The certificate and key.
    ///
    /// Returns:
    ///     The server with TLS on.
    pub fn with_tls(mut self, tls: ServerTls) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Replaces the default 10 second handshake timeout.
    ///
    /// Args:
    ///     timeout: How long a new connection has to finish the WebSocket handshake.
    ///
    /// Returns:
    ///     The server with the new timeout.
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Replaces the default heartbeat.
    ///
    /// Args:
    ///     heartbeat: The ping interval and idle timeout for every connection.
    ///
    /// Returns:
    ///     The server with the new heartbeat.
    pub fn with_heartbeat(mut self, heartbeat: Heartbeat) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    /// Replaces the default payload limit.
    ///
    /// Args:
    ///     max_payload_bytes: The longest payload a publish may carry. Values
    ///         below 1 are treated as 1.
    ///
    /// Returns:
    ///     The server with the new limit.
    pub fn with_max_payload_bytes(mut self, max_payload_bytes: usize) -> Self {
        self.max_payload_bytes = max_payload_bytes.max(1);
        self
    }

    /// Replaces the default outbound queue budget.
    ///
    /// Args:
    ///     limits: The soft and hard byte limits for each connection.
    ///
    /// Returns:
    ///     The server with the new budget.
    pub fn with_queue_limits(mut self, limits: QueueLimits) -> Self {
        self.queue_limits = limits;
        self
    }

    /// The WebSocket settings every connection is accepted with.
    ///
    /// A whole message may be up to twice the payload limit plus
    /// `FRAME_OVERHEAD_BYTES`, enough for a payload at the limit even with
    /// heavy JSON escaping. Anything larger is refused by the WebSocket
    /// layer before it is buffered, and the connection is closed with
    /// `CloseCode::MessageTooBig`.
    ///
    /// Returns:
    ///     The configuration.
    fn websocket_config(&self) -> WebSocketConfig {
        let max = self
            .max_payload_bytes
            .saturating_mul(2)
            .saturating_add(FRAME_OVERHEAD_BYTES);
        WebSocketConfig::default()
            .max_message_size(Some(max))
            .max_frame_size(Some(max))
    }

    /// The limits clients are told about in `authenticated`.
    ///
    /// Returns:
    ///     The window size, session expiry and heartbeat.
    fn limits(&self) -> Limits {
        Limits {
            max_inflight: self.broker.max_inflight() as u64,
            session_expiry_secs: self.broker.session_expiry().as_secs(),
            heartbeat_secs: self.heartbeat.interval.as_secs(),
            idle_timeout_secs: self.heartbeat.idle_timeout.as_secs(),
            max_payload_bytes: self.max_payload_bytes as u64,
            max_topic_bytes: MAX_TOPIC_BYTES as u64,
        }
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
        let scheme = if self.tls.is_some() { "wss" } else { "ws" };
        info!(
            "WebSocket server listening on {scheme}://{}",
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
    /// in its own task, holding a `ConnectionSlot` from `accept` until the
    /// task ends; a connection accepted while the server is at its limit is
    /// told so with `TryAgainLater` once its handshake is done.
    ///
    /// Args:
    ///     listener: The bound listener to accept connections from.
    pub async fn serve(self, listener: TcpListener) {
        let _pruner = AbortOnDrop(tokio::spawn(prune_forever(self.limiters.clone())));
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let slot = ConnectionSlot::take(&self.open_connections);
                    let over_limit = self.max_connections.is_some_and(|max| slot.before >= max);
                    tokio::spawn(self.clone().open_connection(stream, peer, slot, over_limit));
                }
                Err(e) => {
                    warn!(%e, "accepting a connection failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            }
        }
    }

    /// Starts serving an accepted socket: reads the PROXY protocol header
    /// when the server expects one and the peer is a trusted proxy, performs
    /// the TLS handshake when the server has TLS, then hands over to
    /// `run_connection`. The header, TLS and WebSocket handshakes share one
    /// deadline, the handshake timeout from `accept`, so a socket can't hold
    /// its slot by stalling any of them. A trusted peer that sends no valid
    /// header is dropped. The header's address becomes the connection's
    /// source, which `X-Forwarded-For` processing then starts from; a header
    /// without one (a health check) leaves the peer's address.
    ///
    /// Args:
    ///     stream: The accepted TCP connection.
    ///     peer: The connection's remote address.
    ///     slot: This connection's place in the open connection count.
    ///     over_limit: Whether the server was at its connection limit.
    async fn open_connection(
        self,
        stream: TcpStream,
        peer: SocketAddr,
        slot: ConnectionSlot,
        over_limit: bool,
    ) {
        let deadline = Instant::now() + self.handshake_timeout;
        let mut stream = stream;
        let mut source = peer.ip();
        if self.proxy_protocol && self.trusted_proxies.trusts(peer.ip()) {
            match tokio::time::timeout_at(deadline, read_proxy_header(&mut stream)).await {
                Ok(Ok(Some(address))) => source = address,
                Ok(Ok(None)) => {}
                Ok(Err(e)) => {
                    warn!(peer = %pii(peer), %e, "dropping a connection without a valid PROXY header");
                    metrics()
                        .connections_refused
                        .add(1, &attr("reason", "proxy_header"));
                    return;
                }
                Err(_) => {
                    warn!(peer = %pii(peer), "PROXY header timed out");
                    metrics()
                        .connections_refused
                        .add(1, &attr("reason", "proxy_header"));
                    return;
                }
            }
        }
        let Some(tls) = self.tls.clone() else {
            self.run_connection(stream, peer, source, slot, over_limit, deadline)
                .await;
            return;
        };
        match tokio::time::timeout_at(deadline, tls.acceptor().accept(stream)).await {
            Ok(Ok(stream)) => {
                self.run_connection(stream, peer, source, slot, over_limit, deadline)
                    .await;
            }
            Ok(Err(e)) => {
                warn!(peer = %pii(peer), %e, "TLS handshake failed");
                metrics().connections_refused.add(1, &attr("reason", "tls"));
            }
            Err(_) => {
                warn!(peer = %pii(peer), "TLS handshake timed out");
                metrics().connections_refused.add(1, &attr("reason", "tls"));
            }
        }
    }

    /// Runs one connection from handshake to cleanup.
    ///
    /// After the WebSocket handshake a `Client` is registered with the broker
    /// and a send loop starts draining the client's outgoing channel. The
    /// loop then waits for whichever comes first: a frame from the client
    /// (handled by `Session::on_frame`), the heartbeat tick, the token
    /// expiry warning, the token expiry, or the connection's `Outbox`
    /// starting to close (a takeover, a slow consumer, a shutdown). Any frame
    /// received, pongs included, counts as the client being alive. When the server ends the
    /// connection it sends a close frame with the reason's `CloseCode`, then
    /// keeps the connection open for up to `CLOSE_GRACE` until the client
    /// replies with its own close. Dropping it straight away would make a
    /// client with writes still queued (pongs, say) see a broken pipe instead
    /// of the close code. The client is removed from the broker once, by whichever of
    /// this loop and the send loop finishes first.
    ///
    /// The handshake must finish within the handshake timeout. Its
    /// `X-Forwarded-For` headers are kept, and `TrustedProxies::client_address`
    /// decides from them and the peer which address the rate limits key on.
    /// A connection over the server's limit, or over the per-address limit
    /// for its client address, is closed with `TryAgainLater` straight after
    /// the handshake. The header callback allows
    /// `clippy::result_large_err` because tungstenite fixes its error type.
    ///
    /// Args:
    ///     stream: The accepted connection, after TLS if the server has it.
    ///     peer: The connection's remote address.
    ///     source: Where the connection comes from: the PROXY header's
    ///         address when there was one, otherwise the peer's.
    ///     _slot: This connection's place in the open connection count,
    ///         released when the connection ends.
    ///     over_limit: Whether the server was already at its connection
    ///         limit when it was accepted.
    ///     deadline: When the WebSocket handshake must be done by.
    async fn run_connection<S>(
        self,
        stream: S,
        peer: SocketAddr,
        source: IpAddr,
        _slot: ConnectionSlot,
        over_limit: bool,
        deadline: Instant,
    ) where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut forwarded_for = Vec::new();
        #[allow(clippy::result_large_err)]
        let keep_forwarded_for = |request: &Request, response: Response| {
            forwarded_for.extend(
                request
                    .headers()
                    .get_all("x-forwarded-for")
                    .iter()
                    .filter_map(|value| value.to_str().ok().map(str::to_string)),
            );
            Ok::<Response, ErrorResponse>(response)
        };
        let handshake =
            accept_hdr_async_with_config(stream, keep_forwarded_for, Some(self.websocket_config()));
        let mut ws_stream = match tokio::time::timeout_at(deadline, handshake).await {
            Ok(Ok(ws)) => ws,
            Ok(Err(e)) => {
                warn!(peer = %pii(peer), %e, "WebSocket handshake failed");
                metrics()
                    .connections_refused
                    .add(1, &attr("reason", "handshake"));
                return;
            }
            Err(_) => {
                warn!(peer = %pii(peer), "WebSocket handshake timed out");
                metrics()
                    .connections_refused
                    .add(1, &attr("reason", "handshake"));
                return;
            }
        };
        let client_ip = self.trusted_proxies.client_address(source, &forwarded_for);
        if over_limit {
            warn!(peer = %pii(peer), client_ip = %pii(client_ip), "at the connection limit, refusing a connection");
            metrics()
                .connections_refused
                .add(1, &attr("reason", "server_limit"));
            refuse(&mut ws_stream, CloseCode::TryAgainLater).await;
            return;
        }
        let Some(_address_slot) =
            AddressSlot::take(&self.per_address, client_ip, self.max_per_address)
        else {
            warn!(peer = %pii(peer), client_ip = %pii(client_ip), "address at its connection limit, refusing a connection");
            metrics()
                .connections_refused
                .add(1, &attr("reason", "address_limit"));
            refuse(&mut ws_stream, CloseCode::TryAgainLater).await;
            return;
        };
        metrics().connections_opened.add(1, &[]);
        let (ws_sender, mut ws_receiver) = ws_stream.split();
        let (outbox, rx) = Outbox::channel(self.queue_limits);
        let client = Client::new(outbox.clone());
        let client_id = client.id.clone();
        self.broker.with(|broker| broker.register_client(client));

        let cleanup = ClientCleanup::new(self.broker.clone(), client_id.clone());
        spawn_send_loop(ws_sender, rx, client_id.clone(), cleanup.clone());

        let heartbeat = self.heartbeat;
        let session = Session {
            server: self,
            client_id,
            client_ip: client_ip.to_string(),
            outbox,
            auth: Mutex::new(AuthState::default()),
        };
        let mut ticker =
            tokio::time::interval_at(Instant::now() + heartbeat.interval, heartbeat.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut last_heard = Instant::now();
        let ending = loop {
            let (warn_at, expire_at) = session.token_deadlines();
            let never = Instant::now() + Duration::from_secs(365 * 24 * 3600);
            tokio::select! {
                frame = ws_receiver.next() => {
                    let frame = match frame {
                        Some(Ok(frame)) => frame,
                        Some(Err(tungstenite::Error::Capacity(e))) => {
                            warn!(client_id = %session.client_id, %e, "frame too large");
                            break Flow::Close(CloseCode::MessageTooBig);
                        }
                        _ => break Flow::Stop,
                    };
                    last_heard = Instant::now();
                    match session.on_frame(frame).await {
                        Flow::Continue => {}
                        ending => break ending,
                    }
                }
                _ = ticker.tick() => {
                    if last_heard.elapsed() >= heartbeat.idle_timeout {
                        info!(client_id = %session.client_id, "closing an idle connection");
                        break Flow::Close(CloseCode::IdleTimeout);
                    }
                    if session.outbox.send(WsMessage::Ping(Default::default())).is_err() {
                        break Flow::Stop;
                    }
                }
                _ = tokio::time::sleep_until(warn_at.unwrap_or(never)), if warn_at.is_some() => {
                    if session.warn_token_expiring().is_err() {
                        break Flow::Stop;
                    }
                }
                _ = tokio::time::sleep_until(expire_at.unwrap_or(never)), if expire_at.is_some() => {
                    info!(client_id = %session.client_id, "closing a connection whose token expired");
                    break Flow::Close(CloseCode::TokenExpired);
                }
                code = session.outbox.closed() => {
                    info!(client_id = %session.client_id, reason = code.reason(), "connection closed by the server");
                    break Flow::Close(code);
                }
            }
        };
        let reason = match ending {
            Flow::Close(code) => code.reason(),
            _ => "client",
        };
        metrics().connections_closed.add(1, &attr("reason", reason));
        if let Flow::Close(code) = ending {
            session.outbox.close(code);
            let _ = tokio::time::timeout(CLOSE_GRACE, async {
                while let Some(Ok(frame)) = ws_receiver.next().await {
                    if matches!(frame, WsMessage::Close(_)) {
                        break;
                    }
                }
            })
            .await;
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

/// One connection's place in the server's open connection count, given back
/// when dropped.
struct ConnectionSlot {
    /// The shared count.
    open: Arc<AtomicUsize>,
    /// How many connections were open before this one.
    before: usize,
}

impl ConnectionSlot {
    /// Counts a new connection.
    ///
    /// Args:
    ///     open: The server's count of open connections.
    ///
    /// Returns:
    ///     The slot, which records the count before it.
    fn take(open: &Arc<AtomicUsize>) -> Self {
        let before = open.fetch_add(1, Ordering::SeqCst);
        Self {
            open: open.clone(),
            before,
        }
    }
}

impl Drop for ConnectionSlot {
    /// Gives the slot back.
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One connection's place in its client address's connection count, given
/// back when dropped.
struct AddressSlot {
    /// The shared per-address counts.
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    /// The address this connection counts against.
    address: IpAddr,
}

impl AddressSlot {
    /// Counts a connection against its address, unless the address is at the limit.
    ///
    /// Args:
    ///     counts: The server's per-address counts.
    ///     address: The client's address.
    ///     max: The most connections per address, or `None` for no limit.
    ///
    /// Returns:
    ///     The slot, or `None` when the address already holds `max` connections.
    fn take(
        counts: &Arc<Mutex<HashMap<IpAddr, usize>>>,
        address: IpAddr,
        max: Option<usize>,
    ) -> Option<Self> {
        let mut map = counts.lock().unwrap_or_else(PoisonError::into_inner);
        let count = map.entry(address).or_insert(0);
        if max.is_some_and(|max| *count >= max) {
            return None;
        }
        *count += 1;
        Some(Self {
            counts: counts.clone(),
            address,
        })
    }
}

impl Drop for AddressSlot {
    /// Gives the slot back, forgetting the address once it holds no connections.
    fn drop(&mut self) {
        let mut map = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = map.get_mut(&self.address) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.address);
            }
        }
    }
}

/// Closes a connection the server won't serve, with a close code, and waits
/// up to `CLOSE_GRACE` for the client's close reply so it sees the code.
///
/// Args:
///     ws: The connection, after its handshake.
///     code: Why it is closed.
async fn refuse<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, code: CloseCode)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if ws.send(close_message(code)).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(frame)) = ws.next().await {
            if frame.is_close() {
                break;
            }
        }
    })
    .await;
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

/// Whether a connection should keep reading after a frame is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep the connection open.
    Continue,
    /// Close the connection, telling the client why.
    Close(CloseCode),
    /// Stop without a close frame: the client already closed, or the
    /// connection was already removed (for example, taken over).
    Stop,
}

/// What a session knows about its own authentication.
#[derive(Debug, Default)]
struct AuthState {
    /// The `session_id` the client named in `auth`, if any.
    session_id: Option<String>,
    /// When the current token expires, in milliseconds since the Unix epoch.
    expires_at_ms: Option<i64>,
    /// Whether `auth_expiring` was already sent for the current token.
    warned: bool,
}

/// The current time in milliseconds since the Unix epoch.
///
/// Returns:
///     The time.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Turns a wall-clock time into a Tokio deadline.
///
/// Args:
///     at_ms: The time, in milliseconds since the Unix epoch.
///
/// Returns:
///     The deadline, now if the time has passed.
fn deadline(at_ms: i64) -> Instant {
    let wait = u64::try_from(at_ms.saturating_sub(now_ms())).unwrap_or(0);
    Instant::now() + Duration::from_millis(wait)
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
    /// A durable session's stored subscriptions are kept.
    fn run(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            self.broker.cleanup_client(&self.client_id);
        }
    }
}

/// Spawns a task that writes a client's outgoing queue to its WebSocket.
///
/// Frames are written in order. When the `Outbox` closes, the close frame
/// is written (after the backlog, unless it is a slow consumer) and the task
/// stops; it also stops when a write fails or every `Outbox` is dropped.
/// Then it runs `cleanup`.
///
/// Args:
///     ws_sender: The write half of the client's WebSocket.
///     rx: The receiving side of the client's `Outbox`.
///     client_id: The client's broker id, for logging.
///     cleanup: Removes the client from the broker, once.
fn spawn_send_loop<S>(
    mut ws_sender: S,
    mut rx: OutboxReceiver,
    client_id: String,
    cleanup: ClientCleanup,
) where
    S: futures_util::Sink<WsMessage, Error = tungstenite::Error> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(next) = rx.recv().await {
            let (frame, last) = match next {
                Outgoing::Frame(frame) => (frame, false),
                Outgoing::Close(code) => (close_message(code), true),
            };
            if let Err(e) = ws_sender.send(frame).await {
                warn!(client_id = %client_id, %e, "failed to write to socket");
                break;
            }
            if last {
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
    /// The client's address, the key for login and registration limits: the
    /// socket's peer, or the address a trusted proxy forwarded for it.
    client_ip: String,
    /// This connection's outgoing queue. Replies are queued here directly.
    outbox: Outbox,
    /// The session id and token expiry from `auth`.
    auth: Mutex<AuthState>,
}

impl Session {
    /// Handles one WebSocket frame from the client.
    ///
    /// Text frames are parsed and dispatched; a frame that doesn't parse
    /// gets `bad_request` (with its `id`, if one can be read) and the
    /// connection stays open. A close frame from the client ends the loop
    /// without a close frame of our own. Binary, ping and pong frames need
    /// no handling. A handler failure gets an `internal` error and closes
    /// the connection with `CloseCode::InternalError`.
    ///
    /// Args:
    ///     frame: The frame.
    ///
    /// Returns:
    ///     Whether to keep the connection open.
    async fn on_frame(&self, frame: WsMessage) -> Flow {
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Close(_) => return Flow::Stop,
            WsMessage::Binary(_) => {
                warn!(client_id = %self.client_id, "ignoring binary frame");
                return Flow::Continue;
            }
            _ => return Flow::Continue,
        };
        let parsed = match serde_json::from_str::<ClientFrame>(&text) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    client_id = %self.client_id,
                    kind = ?err.classify(),
                    line = err.line(),
                    column = err.column(),
                    bytes = text.len(),
                    "invalid client frame"
                );
                let failed = self.fail(
                    salvage_request_id(&text).as_ref(),
                    ErrorCode::BadRequest,
                    format!("invalid frame: {err}"),
                    None,
                );
                return if failed.is_err() {
                    Flow::Stop
                } else {
                    Flow::Continue
                };
            }
        };
        let outcome = if matches!(parsed.message, ClientMessage::Ack { .. }) {
            self.handle(parsed).await
        } else {
            let kind = parsed.message.kind();
            let span = info_span!(
                "popsub.request",
                otel.name = format!("popsub {kind}"),
                otel.kind = "server",
                otel.status_code = tracing::field::Empty,
                popsub.request.type = kind,
                popsub.client.id = %self.client_id,
                error.type = tracing::field::Empty,
            );
            self.handle(parsed).instrument(span).await
        };
        match outcome {
            Ok(flow) => flow,
            Err(e) => {
                error!(client_id = %self.client_id, "error handling message: {}", e);
                let _ = self.fail(None, ErrorCode::Internal, "internal server error", None);
                Flow::Close(CloseCode::InternalError)
            }
        }
    }

    /// Locks the session's auth state, recovering it if a holder panicked.
    ///
    /// Returns:
    ///     The guard.
    fn auth_state(&self) -> std::sync::MutexGuard<'_, AuthState> {
        self.auth.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Works out when to warn about, and when to act on, the token expiring.
    ///
    /// Returns:
    ///     The warning deadline (once per token, `AUTH_EXPIRY_WARNING`
    ///     before expiry) and the expiry deadline, each `None` when there is
    ///     no token or nothing left to do.
    fn token_deadlines(&self) -> (Option<Instant>, Option<Instant>) {
        let state = self.auth_state();
        let Some(expires_at_ms) = state.expires_at_ms else {
            return (None, None);
        };
        let warning_ms = i64::try_from(AUTH_EXPIRY_WARNING.as_millis()).unwrap_or(i64::MAX);
        let warn_at = (!state.warned).then(|| deadline(expires_at_ms.saturating_sub(warning_ms)));
        (warn_at, Some(deadline(expires_at_ms)))
    }

    /// Sends `auth_expiring` for the current token and remembers that it did.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn warn_token_expiring(&self) -> Result<()> {
        let expires_at_ms = {
            let mut state = self.auth_state();
            state.warned = true;
            state.expires_at_ms
        };
        match expires_at_ms {
            Some(expires_at_ms) => self.reply(None, ServerMessage::AuthExpiring { expires_at_ms }),
            None => Ok(()),
        }
    }

    /// Records the expiry of a newly accepted token.
    ///
    /// Args:
    ///     exp_secs: The token's `exp` claim, in seconds since the Unix epoch.
    fn set_token_expiry(&self, exp_secs: usize) {
        let mut state = self.auth_state();
        state.expires_at_ms = Some(
            i64::try_from(exp_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        );
        state.warned = false;
    }

    /// Dispatches a parsed client frame to its handler.
    ///
    /// Args:
    ///     frame: The frame the client sent, with its request id.
    ///
    /// Returns:
    ///     Whether to keep the connection open.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When a reply can't be queued because the
    ///         connection's outgoing channel is closed.
    async fn handle(&self, frame: ClientFrame) -> Result<Flow> {
        let id = frame.id.as_ref();
        match frame.message {
            ClientMessage::Register { username, password } => {
                self.register(id, &username, &password).await
            }
            ClientMessage::Login { username, password } => {
                self.login(id, &username, &password).await
            }
            ClientMessage::Auth {
                token,
                protocol,
                session_id,
                clean_start,
            } => {
                self.authenticate(id, &token, protocol, session_id.as_deref(), clean_start)
                    .await
            }
            ClientMessage::Subscribe { topic, from } => self.subscribe(id, &topic, from).await,
            ClientMessage::Unsubscribe { topic } => self.unsubscribe(id, &topic).await,
            ClientMessage::Publish {
                topic,
                payload,
                message_id,
                qos,
            } => self.publish(id, &topic, payload, message_id, qos).await,
            ClientMessage::Ack { topic, seq } => self.ack(&topic, seq).await,
            ClientMessage::Ping {} => {
                self.reply(id, ServerMessage::Pong {})?;
                Ok(Flow::Continue)
            }
            admin @ (ClientMessage::CreateUser { .. }
            | ClientMessage::GetUser { .. }
            | ClientMessage::ListUsers {}
            | ClientMessage::SetRole { .. }
            | ClientMessage::Grant { .. }
            | ClientMessage::Revoke { .. }
            | ClientMessage::SetPassword { .. }
            | ClientMessage::SetEnabled { .. }
            | ClientMessage::DeleteUser { .. }
            | ClientMessage::ListTopics {}
            | ClientMessage::GetTopic { .. }
            | ClientMessage::PurgeTopic { .. }) => self.admin(id, admin).await,
        }
    }

    /// Queues a reply on this connection's outgoing channel.
    ///
    /// Args:
    ///     id: The id of the request being answered, if it had one.
    ///     message: The reply.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn reply(&self, id: Option<&RequestId>, message: ServerMessage) -> Result<()> {
        let json = serde_json::to_string(&ServerFrame::reply(id.cloned(), message))?;
        self.outbox
            .send(WsMessage::Text(json.into()))
            .map_err(|e| PopSubError::Client(format!("outgoing queue refused a reply: {e:?}")))
    }

    /// Queues an `error` reply.
    ///
    /// Args:
    ///     id: The id of the request that failed, if it had one.
    ///     code: What went wrong, for programs.
    ///     message: What went wrong, for people.
    ///     retry_after: Seconds to wait before retrying, for limits.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn fail(
        &self,
        id: Option<&RequestId>,
        code: ErrorCode,
        message: impl Into<String>,
        retry_after: Option<u64>,
    ) -> Result<()> {
        metrics().errors.add(1, &attr("code", code.as_str()));
        let span = tracing::Span::current();
        span.record("otel.status_code", "ERROR");
        span.record("error.type", code.as_str());
        self.reply(
            id,
            ServerMessage::Error {
                code,
                message: message.into(),
                retry_after,
            },
        )
    }

    /// Returns the authenticated user, or tells the client to authenticate first.
    ///
    /// Every handler except `register`, `login` and `auth` starts here. A
    /// client that hasn't authenticated gets an `unauthenticated` error and
    /// its connection is closed with `CloseCode::Unauthenticated`; one that
    /// is no longer registered (taken over) is stopped without another close.
    ///
    /// Args:
    ///     id: The id of the request being handled.
    ///     action: What the client tried to do, for the log.
    ///
    /// Returns:
    ///     The username, or the flow to end the connection with.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the error frame can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn require_user(
        &self,
        id: Option<&RequestId>,
        action: &str,
    ) -> Result<std::result::Result<String, Flow>> {
        let state = self.server.broker.with(|broker| {
            broker
                .clients
                .get(&self.client_id)
                .map(|client| (client.authenticated, client.username.clone()))
        });
        match state {
            None => {
                warn!(client_id = %self.client_id, action, "client not registered");
                Ok(Err(Flow::Stop))
            }
            Some((false, _)) => {
                warn!(client_id = %self.client_id, action, "request before authentication");
                self.fail(
                    id,
                    ErrorCode::Unauthenticated,
                    "must authenticate first",
                    None,
                )?;
                Ok(Err(Flow::Close(CloseCode::Unauthenticated)))
            }
            Some((true, username)) => Ok(Ok(username.unwrap_or_default())),
        }
    }

    /// Creates a user account and replies with `registered`.
    ///
    /// Every attempt counts against the registration limit for this address,
    /// whether or not it succeeds. Over the limit, the client gets
    /// `rate_limited` with `retry_after` and the auth service isn't called. A
    /// failed registration gets `registration_failed` with the text from
    /// `registration_error_message`, which doesn't reveal whether the
    /// username is taken. The connection stays open.
    ///
    /// Args:
    ///     id: The request id.
    ///     username: The requested username.
    ///     password: The requested password.
    ///
    /// Returns:
    ///     Always `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn register(
        &self,
        id: Option<&RequestId>,
        username: &str,
        password: &str,
    ) -> Result<Flow> {
        let limiter = &self.server.limiters.register_limiter;
        if !limiter.try_acquire(&self.client_ip) {
            let retry_after = limiter.retry_after(&self.client_ip).unwrap_or(1);
            warn!(client_id = %self.client_id, client_ip = %pii(&self.client_ip), retry_after, "registration rate limited");
            self.fail(
                id,
                ErrorCode::RateLimited,
                format!("too many registration attempts, try again in {retry_after} seconds"),
                Some(retry_after),
            )?;
            return Ok(Flow::Continue);
        }
        match self.server.auth.register_user(username, password).await {
            Ok(()) => {
                info!(client_id = %self.client_id, username = %pii(&username), "registration successful");
                self.reply(id, ServerMessage::Registered {})?;
            }
            Err(e) => {
                warn!(client_id = %self.client_id, username = %pii(username), error = e.kind(), "registration failed");
                self.fail(
                    id,
                    ErrorCode::RegistrationFailed,
                    registration_error_message(&e),
                    None,
                )?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Checks a username and password and replies with `logged_in` and a JWT.
    ///
    /// While this address, or this address and username pair, is locked out,
    /// the client gets `locked_out` with `retry_after` and the auth service
    /// isn't called. Only wrong credentials count as failures, against both
    /// limiters; a success clears the pair's failures but not the address
    /// count, so one valid account can't be used to reset guessing against
    /// others. Every login error is reported as `invalid_credentials`. The
    /// connection stays open.
    ///
    /// Args:
    ///     id: The request id.
    ///     username: The username to log in as.
    ///     password: The password to check.
    ///
    /// Returns:
    ///     Always `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When the reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn login(&self, id: Option<&RequestId>, username: &str, password: &str) -> Result<Flow> {
        let limiters = &self.server.limiters;
        let pair_key = format!("{}|{username}", self.client_ip);
        let lockout = match limiters.ip_login_limiter.check_status(&self.client_ip) {
            LoginStatus::LockedOut { retry_after } => Some(retry_after),
            _ => match limiters.login_limiter.check_status(&pair_key) {
                LoginStatus::LockedOut { retry_after } => Some(retry_after),
                _ => None,
            },
        };
        if let Some(retry_after) = lockout {
            metrics().logins.add(1, &attr("outcome", "locked_out"));
            warn!(client_id = %self.client_id, client_ip = %pii(&self.client_ip), username = %pii(username), retry_after, "login rate limited");
            self.fail(
                id,
                ErrorCode::LockedOut,
                format!("too many login attempts, try again in {retry_after} seconds"),
                Some(retry_after),
            )?;
            return Ok(Flow::Continue);
        }
        match self.server.auth.login(username, password).await {
            Ok(token) => {
                metrics().logins.add(1, &attr("outcome", "success"));
                info!(client_id = %self.client_id, username = %pii(&username), "login successful");
                limiters.login_limiter.record_success(&pair_key);
                self.reply(id, ServerMessage::LoggedIn { token })?;
            }
            Err(e) => {
                metrics()
                    .logins
                    .add(1, &attr("outcome", "invalid_credentials"));
                warn!(client_id = %self.client_id, username = %pii(username), error = e.kind(), "login failed");
                if matches!(e, AuthError::InvalidCredentials) {
                    limiters.login_limiter.record_failure(&pair_key);
                    limiters.ip_login_limiter.record_failure(&self.client_ip);
                }
                self.fail(
                    id,
                    ErrorCode::InvalidCredentials,
                    "invalid credentials",
                    None,
                )?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Validates a JWT, marks the connection as authenticated as its subject,
    /// and attaches the durable session if one is named.
    ///
    /// The reply is `authenticated` with the protocol version and the
    /// server's limits. These get an error and close the connection with the
    /// matching `CloseCode`: an unsupported `protocol` (`unsupported_protocol`),
    /// an invalid or expired token (`auth_failed`), a token issued before the
    /// account's last password change (`auth_failed`), a token whose account is
    /// disabled or deleted (`auth_failed`, closed as `AccountDisabled`), a
    /// user store that can't be read (`internal`, closed as `AuthFailed`), an
    /// invalid `session_id`
    /// (`invalid_session_id`, closed as `AuthFailed`), and a session store
    /// that can't be reached (`session_unavailable`). A client that is no
    /// longer registered is stopped with no reply.
    ///
    /// On a connection that is already authenticated, this only refreshes
    /// the token; see `refresh_token`. The token's expiry is recorded so the
    /// connection loop can warn about it and close the connection when it
    /// passes.
    ///
    /// With a `session_id`, the session is bound to the token's user (another
    /// user's identical id is a different session), any other connection
    /// holding it is closed, and its stored subscriptions are restored after
    /// the reply. Each restored topic is checked against the user's current
    /// permissions; one the user may no longer read is forgotten.
    ///
    /// Args:
    ///     id: The request id.
    ///     token: The JWT from a previous `login`.
    ///     protocol: The protocol version the client speaks; `None` means 1.
    ///     session_id: The durable session to hold, if any.
    ///     clean_start: Whether to discard the session's stored subscriptions.
    ///
    /// Returns:
    ///     `Flow::Continue` when authentication succeeded, otherwise `Flow::Close`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn authenticate(
        &self,
        id: Option<&RequestId>,
        token: &str,
        protocol: Option<u32>,
        session_id: Option<&str>,
        clean_start: bool,
    ) -> Result<Flow> {
        if let Some(version) = protocol
            && version != PROTOCOL_VERSION
        {
            warn!(client_id = %self.client_id, version, "unsupported protocol version");
            self.fail(
                id,
                ErrorCode::UnsupportedProtocol,
                format!("this server speaks protocol {PROTOCOL_VERSION}, not {version}"),
                None,
            )?;
            return Ok(Flow::Close(CloseCode::UnsupportedProtocol));
        }
        let claims = match self.server.auth.validate_token(token) {
            Ok(claims) => claims,
            Err(e) => {
                warn!(client_id = %self.client_id, "authentication failed: {}", e);
                self.fail(id, ErrorCode::AuthFailed, "authentication failed", None)?;
                return Ok(Flow::Close(CloseCode::AuthFailed));
            }
        };
        match self.server.auth.get_user(&claims.sub).await {
            Ok(Some(user)) if user.enabled && user.token_version == claims.ver => {}
            Ok(Some(user)) if user.enabled => {
                warn!(client_id = %self.client_id, username = %pii(&claims.sub), "token issued before a password change");
                self.fail(
                    id,
                    ErrorCode::AuthFailed,
                    "this token was cancelled by a password change; log in again",
                    None,
                )?;
                return Ok(Flow::Close(CloseCode::AuthFailed));
            }
            Ok(_) => {
                warn!(client_id = %self.client_id, username = %pii(&claims.sub), "token for a disabled or deleted account");
                self.fail(
                    id,
                    ErrorCode::AuthFailed,
                    "this account is disabled or no longer exists",
                    None,
                )?;
                return Ok(Flow::Close(CloseCode::AccountDisabled));
            }
            Err(e) => {
                warn!(client_id = %self.client_id, "looking up the token's account failed: {}", e);
                self.fail(id, ErrorCode::Internal, "the user store failed", None)?;
                return Ok(Flow::Close(CloseCode::AuthFailed));
            }
        }
        let current = self.server.broker.with(|broker| {
            broker
                .clients
                .get(&self.client_id)
                .map(|client| (client.authenticated, client.username.clone()))
        });
        match current {
            None => return Ok(Flow::Stop),
            Some((true, Some(user))) => {
                return self.refresh_token(id, &user, &claims.sub, claims.exp);
            }
            Some(_) => {}
        }
        if let Some(session_id) = session_id
            && !valid_session_id(session_id)
        {
            warn!(client_id = %self.client_id, "invalid session_id");
            self.fail(
                id,
                ErrorCode::InvalidSessionId,
                format!(
                    "session_id must be 1 to {MAX_SESSION_ID_CHARS} characters with no control characters"
                ),
                None,
            )?;
            return Ok(Flow::Close(CloseCode::AuthFailed));
        }
        let username = claims.sub;
        let registered = self.server.broker.with(|broker| {
            broker
                .clients
                .get_mut(&self.client_id)
                .map(|client| {
                    client.authenticated = true;
                    client.username = Some(username.clone());
                })
                .is_some()
        });
        if !registered {
            warn!(client_id = %self.client_id, "client not registered");
            return Ok(Flow::Stop);
        }
        self.set_token_expiry(claims.exp);
        self.auth_state().session_id = session_id.map(str::to_string);
        let limits = self.server.limits();
        let Some(session_id) = session_id else {
            info!(client_id = %self.client_id, username = %pii(&username), "authenticated successfully");
            self.reply(
                id,
                ServerMessage::Authenticated {
                    protocol: PROTOCOL_VERSION,
                    session_id: None,
                    resumed: Vec::new(),
                    limits,
                },
            )?;
            return Ok(Flow::Continue);
        };

        let key = session_key(&username, session_id);
        let stored = match self
            .server
            .broker
            .attach_session(&self.client_id, &key, clean_start)
            .await
        {
            Ok(stored) => stored,
            Err(e) => {
                error!(client_id = %self.client_id, username = %pii(&username), %e, "attaching a session failed");
                self.fail(
                    id,
                    ErrorCode::SessionUnavailable,
                    "can't open the session, try again",
                    None,
                )?;
                return Ok(Flow::Close(CloseCode::SessionUnavailable));
            }
        };
        let mut resumed = Vec::new();
        for subscription in stored {
            if self
                .server
                .auth
                .can_subscribe(&username, &subscription.topic)
                .await
            {
                resumed.push(subscription.topic);
            } else {
                warn!(client_id = %self.client_id, username = %pii(&username), topic = %subscription.topic, "dropping a stored subscription the user may no longer read");
                if let Err(e) = self
                    .server
                    .broker
                    .forget_subscription(&key, &subscription.topic)
                    .await
                {
                    warn!(%e, "forgetting a stored subscription failed");
                }
            }
        }
        info!(client_id = %self.client_id, username = %pii(&username), session_id = %pii(session_id), clean_start, resumed = resumed.len(), "authenticated with a session");
        self.reply(
            id,
            ServerMessage::Authenticated {
                protocol: PROTOCOL_VERSION,
                session_id: Some(session_id.to_string()),
                resumed: resumed.clone(),
                limits,
            },
        )?;
        for topic in resumed {
            if let Err(e) = self
                .server
                .broker
                .subscribe(&topic, &self.client_id, StartFrom::Resume, None)
                .await
            {
                error!(client_id = %self.client_id, topic, %e, "restoring a subscription failed");
                self.fail(
                    None,
                    ErrorCode::StorageUnavailable,
                    format!("can't restore the subscription to '{topic}', subscribe again"),
                    None,
                )?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Accepts a new token on an already authenticated connection.
    ///
    /// The token must be for the same user, or the client gets `auth_failed`
    /// and keeps its current authentication. Otherwise its expiry replaces
    /// the old one and the reply is `authenticated` with the current session
    /// id and an empty `resumed`. Nothing else about the connection changes:
    /// `session_id` and `clean_start` in the frame are ignored.
    ///
    /// Args:
    ///     id: The request id.
    ///     current_user: The user the connection is authenticated as.
    ///     token_user: The new token's subject.
    ///     exp_secs: The new token's `exp` claim.
    ///
    /// Returns:
    ///     Always `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    fn refresh_token(
        &self,
        id: Option<&RequestId>,
        current_user: &str,
        token_user: &str,
        exp_secs: usize,
    ) -> Result<Flow> {
        if token_user != current_user {
            warn!(client_id = %self.client_id, current_user = %pii(&current_user), token_user = %pii(&token_user), "refusing a token for a different user");
            self.fail(
                id,
                ErrorCode::AuthFailed,
                "the token is for a different user",
                None,
            )?;
            return Ok(Flow::Continue);
        }
        self.set_token_expiry(exp_secs);
        let session_id = self.auth_state().session_id.clone();
        info!(client_id = %self.client_id, current_user = %pii(&current_user), "token refreshed");
        self.reply(
            id,
            ServerMessage::Authenticated {
                protocol: PROTOCOL_VERSION,
                session_id,
                resumed: Vec::new(),
                limits: self.server.limits(),
            },
        )?;
        Ok(Flow::Continue)
    }

    /// Subscribes the connection to a topic if its user may read it.
    ///
    /// An invalid topic name gets `invalid_topic` and a user without
    /// permission gets `not_authorized`; both stay connected.
    /// On success the broker sends `subscribed` (carrying this request's id)
    /// with the replay range, `truncated` if some of it was already trimmed,
    /// and then the history `from` asks for. For a durable session the
    /// subscription and its cursor are stored. If the log or the session
    /// store can't be reached, the client gets `storage_unavailable`, is not
    /// subscribed, and may try again.
    ///
    /// Args:
    ///     id: The request id.
    ///     topic: The topic to subscribe to.
    ///     from: Where replay starts.
    ///
    /// Returns:
    ///     A closing flow when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn subscribe(
        &self,
        id: Option<&RequestId>,
        topic: &str,
        from: StartFrom,
    ) -> Result<Flow> {
        let username = match self.require_user(id, "subscribe")? {
            Ok(username) => username,
            Err(flow) => return Ok(flow),
        };
        if !valid_topic(topic) {
            self.fail(id, ErrorCode::InvalidTopic, invalid_topic_message(), None)?;
            return Ok(Flow::Continue);
        }
        if !self.server.auth.can_subscribe(&username, topic).await {
            warn!(client_id = %self.client_id, username = %pii(&username), topic, "subscribe denied");
            self.fail(
                id,
                ErrorCode::NotAuthorized,
                format!("not authorized to subscribe to '{topic}'"),
                None,
            )?;
            return Ok(Flow::Continue);
        }
        match self
            .server
            .broker
            .subscribe(topic, &self.client_id, from, id.cloned())
            .await
        {
            Ok(outcome) => {
                info!(
                    client_id = %self.client_id,
                    username = %pii(&username),
                    topic,
                    replay_from = outcome.replay_from,
                    replay_to = outcome.replay_to,
                    replayed = outcome.replayed,
                    truncated = outcome.truncated,
                    "subscribed"
                );
            }
            Err(e) => {
                error!(client_id = %self.client_id, username = %pii(&username), topic, %e, "replay failed, not subscribed");
                self.fail(
                    id,
                    ErrorCode::StorageUnavailable,
                    format!("can't read the history of '{topic}', not subscribed"),
                    None,
                )?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Unsubscribes the connection from a topic and replies with `unsubscribed`.
    ///
    /// No permission check is made, since leaving a topic grants nothing.
    /// Leaving a topic the client isn't on still gets `unsubscribed`. For a
    /// durable session the stored subscription is removed too; if that
    /// fails the client gets `storage_unavailable`, live delivery has still
    /// stopped, and the subscription may come back on reconnect.
    ///
    /// Args:
    ///     id: The request id.
    ///     topic: The topic to leave.
    ///
    /// Returns:
    ///     A closing flow when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn unsubscribe(&self, id: Option<&RequestId>, topic: &str) -> Result<Flow> {
        let username = match self.require_user(id, "unsubscribe")? {
            Ok(username) => username,
            Err(flow) => return Ok(flow),
        };
        if let Err(e) = self.server.broker.unsubscribe(topic, &self.client_id).await {
            error!(client_id = %self.client_id, username = %pii(&username), topic, %e, "removing a stored subscription failed");
            self.fail(
                id,
                ErrorCode::StorageUnavailable,
                format!("unsubscribed from '{topic}', but the session may restore it"),
                None,
            )?;
            return Ok(Flow::Continue);
        }
        info!(client_id = %self.client_id, username = %pii(&username), topic, "unsubscribed");
        self.reply(
            id,
            ServerMessage::Unsubscribed {
                topic: topic.to_string(),
            },
        )?;
        Ok(Flow::Continue)
    }

    /// Publishes a payload to a topic if the user may write to it and is within its rate limit.
    ///
    /// The topic name, payload size and `message_id` are checked first
    /// (`invalid_topic`, `payload_too_large`, `bad_request`), then the topic
    /// permission, then the rate limit, so neither a malformed nor a denied
    /// publish uses up the user's allowance. A denied publish gets
    /// `not_authorized` and a limited one `rate_limited` with `retry_after`;
    /// both keep the connection open. The broker sets the publish time,
    /// generates an id when `message_id` is missing, stores the record and
    /// delivers it. A repeated `message_id` from the same user is stored and
    /// delivered once. A QoS 1 publisher gets `published` with the id and
    /// `seq` once the record is stored at the configured durability (the
    /// original `seq` for a retry). If storage fails, the publisher gets
    /// `storage_unavailable` whatever the QoS, so it knows nothing was sent.
    ///
    /// Args:
    ///     id: The request id.
    ///     topic: The topic to publish to.
    ///     payload: The message body.
    ///     message_id: The publisher's idempotency key.
    ///     qos: The delivery level, 0 or 1. Defaults to 0.
    ///
    /// Returns:
    ///     A closing flow when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn publish(
        &self,
        id: Option<&RequestId>,
        topic: &str,
        payload: String,
        message_id: Option<String>,
        qos: Option<u8>,
    ) -> Result<Flow> {
        let username = match self.require_user(id, "publish")? {
            Ok(username) => username,
            Err(flow) => return Ok(flow),
        };
        if !valid_topic(topic) {
            self.fail(id, ErrorCode::InvalidTopic, invalid_topic_message(), None)?;
            return Ok(Flow::Continue);
        }
        if payload.len() > self.server.max_payload_bytes {
            self.fail(
                id,
                ErrorCode::PayloadTooLarge,
                format!(
                    "the payload is {} bytes, the limit is {}",
                    payload.len(),
                    self.server.max_payload_bytes
                ),
                None,
            )?;
            return Ok(Flow::Continue);
        }
        if message_id
            .as_deref()
            .is_some_and(|m| m.is_empty() || m.chars().count() > MAX_MESSAGE_ID_CHARS)
        {
            self.fail(
                id,
                ErrorCode::BadRequest,
                format!("message_id must be 1 to {MAX_MESSAGE_ID_CHARS} characters"),
                None,
            )?;
            return Ok(Flow::Continue);
        }
        if !self.server.auth.can_publish(&username, topic).await {
            warn!(client_id = %self.client_id, username = %pii(&username), topic, "publish denied");
            self.fail(
                id,
                ErrorCode::NotAuthorized,
                format!("not authorized to publish to '{topic}'"),
                None,
            )?;
            return Ok(Flow::Continue);
        }
        let limiter = &self.server.limiters.message_limiter;
        if !limiter.try_acquire(&username) {
            let retry_after = limiter.retry_after(&username).unwrap_or(1);
            warn!(client_id = %self.client_id, username = %pii(&username), retry_after, "message rate limited");
            self.fail(
                id,
                ErrorCode::RateLimited,
                "too many messages",
                Some(retry_after),
            )?;
            return Ok(Flow::Continue);
        }
        let qos = qos.unwrap_or(0);
        let record = NewRecord {
            message_id: message_id.unwrap_or_default(),
            publisher: username.clone(),
            payload,
            qos,
            published_at_ms: 0,
        };
        match self.server.broker.publish(topic, record).await {
            Ok(published) => {
                info!(
                    client_id = %self.client_id,
                    username = %pii(&username),
                    topic,
                    seq = published.seq,
                    duplicate = published.duplicate,
                    "published"
                );
                if qos == 1 {
                    self.reply(
                        id,
                        ServerMessage::Published {
                            message_id: published.message_id,
                            seq: published.seq,
                        },
                    )?;
                }
            }
            Err(e) => {
                error!(client_id = %self.client_id, username = %pii(&username), topic, qos, %e, "publish not stored");
                self.fail(
                    id,
                    ErrorCode::StorageUnavailable,
                    "storage unavailable, nothing was published",
                    None,
                )?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Acknowledges a QoS 1 record delivered to this connection. No reply.
    ///
    /// Only a record in flight to this connection on this topic is accepted;
    /// anything else (a record sent to another client, a repeat ACK) is
    /// logged and ignored, so one client can never clear another's delivery.
    ///
    /// Args:
    ///     topic: The topic the record was delivered on.
    ///     seq: The record's `seq`.
    ///
    /// Returns:
    ///     A closing flow when the client isn't authenticated, otherwise
    ///     `Flow::Continue`.
    ///
    /// Raises:
    ///     PopSubError::Serialization: When a reply can't be serialised.
    ///     PopSubError::Client: When the outgoing channel is closed.
    async fn ack(&self, topic: &str, seq: u64) -> Result<Flow> {
        if let Err(flow) = self.require_user(None, "ack")? {
            return Ok(flow);
        }
        if !self.server.broker.ack(&self.client_id, topic, seq).await {
            info!(client_id = %self.client_id, topic, seq, "ignored an ACK for a record not in flight to this client");
        }
        Ok(Flow::Continue)
    }
}

/// Reads the request id out of a frame that didn't parse, so the
/// `bad_request` error can still be matched to it.
///
/// Args:
///     text: The frame as received.
///
/// Returns:
///     The id, when the text is a JSON object with a valid `id`.
fn salvage_request_id(text: &str) -> Option<RequestId> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::from_value(value.get("id")?.clone()).ok()
}

/// Checks a topic name a client wants to subscribe or publish to.
///
/// Args:
///     topic: The topic name.
///
/// Returns:
///     True for 1 to `MAX_TOPIC_BYTES` bytes with no control characters and
///     no `+` or `#`, which are kept for wildcard patterns.
fn valid_topic(topic: &str) -> bool {
    (1..=MAX_TOPIC_BYTES).contains(&topic.len())
        && !topic
            .chars()
            .any(|c| c.is_control() || c == '+' || c == '#')
}

/// The error text for an invalid topic name.
///
/// Returns:
///     A message stating the rules.
fn invalid_topic_message() -> String {
    format!("topic names are 1 to {MAX_TOPIC_BYTES} bytes, with no control characters, '+' or '#'")
}

/// Checks a client-chosen session id.
///
/// Args:
///     id: The `session_id` from an `auth` frame.
///
/// Returns:
///     True for 1 to `MAX_SESSION_ID_CHARS` characters with no control characters.
fn valid_session_id(id: &str) -> bool {
    let chars = id.chars().count();
    (1..=MAX_SESSION_ID_CHARS).contains(&chars) && !id.chars().any(char::is_control)
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
/// registration role parsing, session id checks and rate limiter pruning.
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

    /// Checks that topic names must be non-empty, short enough and free of
    /// control and wildcard characters.
    #[test]
    fn topic_names_are_checked() {
        for topic in [
            "chat",
            "metrics/cpu",
            "a.b-c_d",
            "日本語",
            &"x".repeat(MAX_TOPIC_BYTES),
        ] {
            assert!(valid_topic(topic), "{topic}");
        }
        for topic in [
            "",
            "a/+/b",
            "a/#",
            "tab\there",
            &"x".repeat(MAX_TOPIC_BYTES + 1),
        ] {
            assert!(!valid_topic(topic), "{topic}");
        }
    }

    /// Checks that session ids must be short, non-empty and free of control characters.
    #[test]
    fn session_ids_are_checked() {
        assert!(valid_session_id("phone-1"));
        assert!(valid_session_id(&"x".repeat(MAX_SESSION_ID_CHARS)));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id(&"x".repeat(MAX_SESSION_ID_CHARS + 1)));
        assert!(!valid_session_id("bad\nid"));
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
