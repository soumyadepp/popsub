//! How a `Client` connects: where, as whom, and how it behaves when things go wrong.

use std::time::Duration;

/// How the client proves who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A username and password. The client logs in on every connection and
    /// refreshes its token on its own before it expires.
    Password {
        /// The username.
        username: String,
        /// The password.
        password: String,
    },
    /// A JWT obtained elsewhere. The client can't refresh it: when it
    /// expires the client stops with `Error::Closed`, after emitting
    /// `Event::AuthExpiring`.
    Token(String),
}

impl Credentials {
    /// Builds password credentials.
    ///
    /// Args:
    ///     username: The username.
    ///     password: The password.
    ///
    /// Returns:
    ///     The credentials.
    pub fn password(username: impl Into<String>, password: impl Into<String>) -> Self {
        Credentials::Password {
            username: username.into(),
            password: password.into(),
        }
    }
}

/// When QoS 1 messages are acknowledged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AckMode {
    /// As soon as the client hands the message to the application. Simple,
    /// but a message the application was processing when it crashed isn't
    /// redelivered.
    #[default]
    Auto,
    /// When the application calls `Message::ack`. A message that is never
    /// acknowledged is redelivered after a reconnect (to a durable session),
    /// and stalls its subscription once the server runs out of retries.
    Manual,
}

/// How the client waits between reconnection attempts.
///
/// The delay doubles from `initial` up to `max`, and each wait is a random
/// time between zero and that delay, so many clients reconnecting after a
/// restart don't all arrive at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// The delay before the first retry.
    pub initial: Duration,
    /// The longest delay between retries.
    pub max: Duration,
    /// Give up after this many attempts in a row; `None` keeps trying.
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectPolicy {
    /// Builds the default policy: 250 ms doubling up to 30 seconds, forever.
    ///
    /// Returns:
    ///     The default policy.
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(30),
            max_attempts: None,
        }
    }
}

impl ReconnectPolicy {
    /// The longest wait before an attempt, before jitter.
    ///
    /// Args:
    ///     attempt: How many attempts in a row have failed, from 0.
    ///
    /// Returns:
    ///     `initial` doubled `attempt` times, capped at `max`.
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.min(30));
        self.initial.saturating_mul(factor).min(self.max)
    }
}

/// Everything a `Client` needs to connect.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The server's WebSocket URL, such as `ws://127.0.0.1:8080`.
    pub url: String,
    /// Who to connect as.
    pub credentials: Credentials,
    /// A durable session to hold, so subscriptions and unacknowledged
    /// messages survive disconnects. `None` is an ephemeral connection.
    pub session_id: Option<String>,
    /// Discard the session's stored subscriptions on the first connection.
    /// Never applied on reconnects.
    pub clean_start: bool,
    /// When QoS 1 messages are acknowledged.
    pub ack_mode: AckMode,
    /// How to reconnect.
    pub reconnect: ReconnectPolicy,
    /// How long a request (subscribe, confirmed publish, connect) may take,
    /// reconnects included, before it fails with `Error::Timeout`.
    pub request_timeout: Duration,
    /// How many messages each subscription buffers for the application. When
    /// one is full the client stops reading the socket until there is room,
    /// and the server paces delivery.
    pub subscription_buffer: usize,
}

impl ClientConfig {
    /// Builds a configuration with the defaults: ephemeral, automatic
    /// acknowledgement, the default reconnect policy, 30 second requests and
    /// 1024 buffered messages per subscription.
    ///
    /// Args:
    ///     url: The server's WebSocket URL.
    ///     credentials: Who to connect as.
    ///
    /// Returns:
    ///     The configuration.
    pub fn new(url: impl Into<String>, credentials: Credentials) -> Self {
        Self {
            url: url.into(),
            credentials,
            session_id: None,
            clean_start: false,
            ack_mode: AckMode::Auto,
            reconnect: ReconnectPolicy::default(),
            request_timeout: Duration::from_secs(30),
            subscription_buffer: 1024,
        }
    }

    /// Holds a durable session.
    ///
    /// Args:
    ///     session_id: The session's id, 1 to 128 characters.
    ///
    /// Returns:
    ///     The configuration.
    pub fn session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Sets when QoS 1 messages are acknowledged.
    ///
    /// Args:
    ///     ack_mode: The mode.
    ///
    /// Returns:
    ///     The configuration.
    pub fn ack_mode(mut self, ack_mode: AckMode) -> Self {
        self.ack_mode = ack_mode;
        self
    }

    /// Sets how the client reconnects.
    ///
    /// Args:
    ///     reconnect: The policy.
    ///
    /// Returns:
    ///     The configuration.
    pub fn reconnect(mut self, reconnect: ReconnectPolicy) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Sets how long a request may take.
    ///
    /// Args:
    ///     timeout: The limit.
    ///
    /// Returns:
    ///     The configuration.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }
}
