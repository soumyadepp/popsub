//! What the application receives: messages, subscriptions and events.

use popsub_protocol::CloseCode;
use tokio::sync::mpsc;

use crate::driver::Command;

/// A message delivered on a subscription.
#[derive(Debug)]
pub struct Message {
    /// The topic it was published to.
    pub topic: String,
    /// Its position in the topic. Each `(topic, seq)` reaches the
    /// application at most once per client.
    pub seq: u64,
    /// The publisher's id for it.
    pub message_id: String,
    /// The body.
    pub payload: String,
    /// 0 or 1.
    pub qos: u8,
    /// When it was published, in milliseconds since the Unix epoch.
    pub published_at_ms: i64,
    /// Where `ack` sends its acknowledgement, for QoS 1 messages in
    /// `AckMode::Manual`.
    pub(crate) acker: Option<mpsc::UnboundedSender<Command>>,
}

impl Message {
    /// Acknowledges the message, so the server stops resending it and a
    /// durable session's cursor can move past it.
    ///
    /// Does nothing for QoS 0 messages and in `AckMode::Auto`, where the
    /// client already acknowledged it. Calling it more than once is harmless.
    /// If the connection is down, the acknowledgement is sent after the
    /// client reconnects.
    pub fn ack(&self) {
        if let Some(acker) = &self.acker {
            let _ = acker.send(Command::Ack {
                topic: self.topic.clone(),
                seq: self.seq,
            });
        }
    }
}

/// The messages of one subscribed topic, in `seq` order.
///
/// Dropping it unsubscribes.
#[derive(Debug)]
pub struct Subscription {
    /// The topic.
    pub(crate) topic: String,
    /// Where the client puts the topic's messages.
    pub(crate) messages: mpsc::Receiver<Message>,
    /// The client's command queue, to unsubscribe on drop.
    pub(crate) commands: mpsc::UnboundedSender<Command>,
    /// Which subscription this is, so dropping an old one for a topic that
    /// was subscribed again doesn't remove the new one.
    pub(crate) generation: u64,
}

impl Subscription {
    /// The subscribed topic.
    ///
    /// Returns:
    ///     The topic name.
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Waits for the next message.
    ///
    /// Returns:
    ///     The message, or `None` once the client has stopped for good or
    ///     the server ended the subscription (see `Event::Unsubscribed`).
    pub async fn next(&mut self) -> Option<Message> {
        self.messages.recv().await
    }
}

impl Drop for Subscription {
    /// Unsubscribes from the topic.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Forget {
            topic: self.topic.clone(),
            generation: self.generation,
        });
    }
}

/// Something that happened to the connection or a subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A connection is up and authenticated.
    Connected {
        /// Topics the server restored for the durable session.
        resumed: Vec<String>,
    },
    /// The connection went down.
    Disconnected {
        /// The server's close code, if it closed the connection.
        code: Option<CloseCode>,
        /// Whether the client will try to reconnect.
        reconnecting: bool,
    },
    /// Messages on a topic were trimmed before the client got them.
    Truncated {
        /// The topic.
        topic: String,
        /// The oldest `seq` still available; delivery continues from here.
        first_available: u64,
    },
    /// A QoS 1 message ran out of retries; the server paused the topic for
    /// this client until it acknowledges something or subscribes again.
    Stalled {
        /// The topic.
        topic: String,
        /// The message that was never acknowledged.
        seq: u64,
    },
    /// The token will expire soon. With password credentials the client
    /// refreshes it on its own; with a bare token the connection closes when
    /// it expires.
    AuthExpiring {
        /// When it expires, in milliseconds since the Unix epoch.
        expires_at_ms: i64,
    },
    /// The client got a new token and the connection carries on.
    AuthRefreshed,
    /// The server ended a subscription, because the user may no longer read
    /// the topic. Its `Subscription` ends, and the client won't resubscribe.
    Unsubscribed {
        /// The topic.
        topic: String,
    },
    /// The server reported an error that answers no request of the
    /// application, such as a failed QoS 0 publish.
    ServerError {
        /// What went wrong.
        code: popsub_protocol::ErrorCode,
        /// What went wrong, for people.
        message: String,
    },
}
