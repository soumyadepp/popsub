//! PopSub's wire: JSON frames over a WebSocket, for the benchmark's publishers and subscribers.
//!
//! Frames are written from a prepared template and read into a struct that borrows from the
//! frame's text, so the load generator does as little work per message as it can.

use std::sync::Arc;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::watch;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

use crate::conn::Socket;
use crate::load::{
    Clock, Outlet, Publisher, PublisherSpec, Replies, SubscriberSpec, SubscriberStats, Totals,
    Window, pace, padding, stamp,
};

/// How many frames a subscriber handles before flushing its acknowledgements.
const SUBSCRIBER_BATCH: usize = 256;

/// The fields of a server frame the load generator looks at.
///
/// Borrowed from the frame's text, so reading a delivery allocates nothing. The benchmark's own
/// topics and payloads contain no JSON escapes, which is what makes borrowing them possible.
#[derive(Debug, Deserialize)]
struct Incoming<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    topic: Option<&'a str>,
    #[serde(default)]
    payload: Option<&'a str>,
}

/// A `publish` frame with everything but the send time and request id already written.
#[derive(Debug, Clone)]
pub struct PublishTemplate {
    head: String,
    tail: String,
}

impl PublishTemplate {
    /// Prepares the frame for one topic.
    ///
    /// Args:
    ///     topic: The topic to publish to.
    ///     qos: 0 or 1.
    ///     payload_bytes: The payload's length.
    ///
    /// Returns:
    ///     The template.
    pub fn new(topic: &str, qos: u8, payload_bytes: usize) -> Self {
        let topic = serde_json::Value::String(topic.to_string());
        Self {
            head: format!(r#"{{"type":"publish","topic":{topic},"qos":{qos},"payload":""#),
            tail: format!(r#"{}""#, padding(payload_bytes)),
        }
    }

    /// Writes one frame.
    ///
    /// Args:
    ///     stamp_ns: The send time to put at the start of the payload.
    ///     id: A request id, for publishes that expect a `published` reply.
    ///
    /// Returns:
    ///     The frame as JSON text.
    pub fn render(&self, stamp_ns: u64, id: Option<u64>) -> String {
        let mut frame = String::with_capacity(self.head.len() + self.tail.len() + 40);
        frame.push_str(&self.head);
        frame.push_str(&stamp(stamp_ns));
        frame.push_str(&self.tail);
        if let Some(id) = id {
            frame.push_str(&format!(r#","id":{id}"#));
        }
        frame.push('}');
        frame
    }
}

/// Reads the sequence number out of a delivery.
///
/// Args:
///     text: A server frame's JSON text.
///
/// Returns:
///     The `seq` when the frame is a `message`, otherwise `None`.
pub fn delivery_seq(text: &str) -> Option<u64> {
    let incoming = serde_json::from_str::<Incoming>(text).ok()?;
    (incoming.kind == "message")
        .then_some(incoming.seq)
        .flatten()
}

/// The writing half of a publisher's WebSocket, with one frame template per topic.
struct WebSocketOutlet {
    sink: SplitSink<Socket, Message>,
    templates: Vec<PublishTemplate>,
}

impl Outlet for WebSocketOutlet {
    /// Queues one `publish` frame on the socket.
    ///
    /// Args:
    ///     topic: Which template to use.
    ///     stamp_ns: The send time to write into the payload.
    ///     id: A request id, for QoS 1 publishes.
    ///
    /// Returns:
    ///     False when the socket has failed.
    async fn feed(&mut self, topic: usize, stamp_ns: u64, id: Option<u64>) -> bool {
        let Some(template) = self.templates.get(topic) else {
            return false;
        };
        self.sink
            .feed(Message::text(template.render(stamp_ns, id)))
            .await
            .is_ok()
    }

    /// Flushes the socket.
    ///
    /// Returns:
    ///     False when the socket has failed.
    async fn flush(&mut self) -> bool {
        self.sink.flush().await.is_ok()
    }
}

/// Publishes to PopSub until the window ends.
///
/// Confirmed publishes are QoS 1 and count the `published` replies.
///
/// Args:
///     socket: An authenticated connection that may publish to the topics.
///     spec: The topics, pacing and payload size.
///     clock: The run's clock.
///     window: The measurement window.
///     totals: The group's totals.
///
/// Returns:
///     The stopped publisher, to be kept until the run has drained.
pub async fn publish(
    socket: Socket,
    spec: PublisherSpec,
    clock: Clock,
    window: Window,
    totals: Arc<Totals>,
) -> Publisher {
    let (sink, stream) = socket.split();
    let qos = u8::from(spec.mode.in_flight() > 0);
    let mut outlet = WebSocketOutlet {
        sink,
        templates: spec
            .topics
            .iter()
            .map(|topic| PublishTemplate::new(topic, qos, spec.payload_bytes))
            .collect(),
    };
    let replies = Replies::new(spec.mode);
    let reader = tokio::spawn(read_replies(stream, clock, window, replies.clone()));
    let sent = pace(&mut outlet, &spec, clock, window, &totals, &replies).await;
    Publisher::new(sent, replies, spec.mode, reader, outlet.sink)
}

/// Reads a publisher's replies until its connection closes.
///
/// Args:
///     stream: The reading half of the publisher's socket.
///     clock: The run's clock.
///     window: The measurement window.
///     replies: Where `published` and `error` frames are counted.
async fn read_replies(
    mut stream: SplitStream<Socket>,
    clock: Clock,
    window: Window,
    replies: Replies,
) {
    while let Some(Ok(frame)) = stream.next().await {
        let Message::Text(text) = frame else {
            continue;
        };
        let Ok(incoming) = serde_json::from_str::<Incoming>(text.as_str()) else {
            continue;
        };
        match incoming.kind {
            "published" => replies.confirmed(clock, window),
            "error" => replies.error(),
            _ => {}
        }
    }
}

/// Reads deliveries on a subscribed connection until told to stop.
///
/// Reads as many frames as are ready, up to `SUBSCRIBER_BATCH`, then flushes the
/// acknowledgements for them in one write.
///
/// Args:
///     socket: A connection that is already subscribed.
///     spec: Whether to acknowledge, record latency or read slowly.
///     clock: The run's clock.
///     window: The measurement window.
///     totals: The group's totals, where every delivery is counted. `None` for a subscriber
///         that isn't expected to keep up.
///     stop: Becomes true when the run has drained.
///
/// Returns:
///     What the subscriber counted.
pub async fn subscribe(
    socket: Socket,
    spec: SubscriberSpec,
    clock: Clock,
    window: Window,
    totals: Option<Arc<Totals>>,
    mut stop: watch::Receiver<bool>,
) -> SubscriberStats {
    let (mut sink, mut stream) = socket.split();
    let mut stats = SubscriberStats::new(&spec);
    loop {
        let first = tokio::select! {
            biased;
            _ = stop.changed() => break,
            frame = stream.next() => frame,
        };
        let mut next = first;
        let mut handled = 0;
        let mut acknowledged = false;
        loop {
            let Some(Ok(frame)) = next else {
                return stats;
            };
            if let Message::Text(text) = frame
                && let Some((topic, seq)) =
                    record(&mut stats, text.as_str(), clock, window, totals.as_deref())
            {
                if spec.acknowledge {
                    let ack = format!(r#"{{"type":"ack","topic":"{topic}","seq":{seq}}}"#);
                    if sink.feed(Message::text(ack)).await.is_err() {
                        return stats;
                    }
                    acknowledged = true;
                }
                if let Some(delay) = spec.delay {
                    sleep(delay).await;
                }
            }
            handled += 1;
            if handled >= SUBSCRIBER_BATCH {
                break;
            }
            match stream.next().now_or_never() {
                Some(frame) => next = frame,
                None => break,
            }
        }
        if acknowledged && sink.flush().await.is_err() {
            return stats;
        }
    }
    stats
}

/// Counts one frame from the server.
///
/// Args:
///     stats: The subscriber's counters.
///     text: The frame's JSON text.
///     clock: The run's clock.
///     window: The measurement window.
///     totals: The group's totals, when this subscriber counts towards them.
///
/// Returns:
///     The delivery's topic and `seq` when the frame is a `message`, otherwise `None`.
fn record<'a>(
    stats: &mut SubscriberStats,
    text: &'a str,
    clock: Clock,
    window: Window,
    totals: Option<&Totals>,
) -> Option<(&'a str, u64)> {
    let Ok(incoming) = serde_json::from_str::<Incoming<'a>>(text) else {
        stats.errors += 1;
        return None;
    };
    match incoming.kind {
        "message" => {
            let payload = incoming.payload.unwrap_or_default();
            stats.delivery(payload.as_bytes(), clock, window, totals);
            incoming.topic.zip(incoming.seq)
        }
        "error" => {
            stats.errors += 1;
            None
        }
        _ => None,
    }
}
