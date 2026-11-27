//! MQTT's wire: version 3.1.1 packets over TCP, for running the same loads against Mosquitto.
//!
//! Unconfirmed publishes are QoS 0. Confirmed publishes are QoS 1: the broker answers each
//! with a `PUBACK`, and subscribers subscribe at QoS 1 and send a `PUBACK` for every delivery.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;
use tokio::time::sleep;

use crate::load::{
    Clock, Outlet, Publisher, PublisherSpec, Replies, SubscriberSpec, SubscriberStats, Totals,
    Window, pace, padding, stamp,
};

/// The size of each connection's read and write buffers.
const BUFFER_BYTES: usize = 64 * 1024;

/// The packet type in the top four bits of a packet's first byte.
const PUBLISH: u8 = 3;

/// The packet type of a publish acknowledgement.
const PUBACK: u8 = 4;

/// Numbers the client ids, which a broker requires to be unique.
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(0);

/// One connection to an MQTT broker.
pub struct MqttConn {
    reader: BufReader<OwnedReadHalf>,
    writer: BufWriter<OwnedWriteHalf>,
}

/// Appends a packet's remaining length in MQTT's variable-length encoding.
///
/// Args:
///     packet: The packet being built.
///     length: The number of bytes that follow the length itself.
pub fn push_remaining_length(packet: &mut Vec<u8>, mut length: usize) {
    loop {
        let mut byte = (length % 128) as u8;
        length /= 128;
        if length > 0 {
            byte |= 0x80;
        }
        packet.push(byte);
        if length == 0 {
            return;
        }
    }
}

/// Appends a string with its two-byte length in front, as MQTT writes them.
///
/// Args:
///     packet: The packet being built.
///     text: The string, shorter than 65,536 bytes.
fn push_string(packet: &mut Vec<u8>, text: &str) {
    packet.extend_from_slice(&(text.len() as u16).to_be_bytes());
    packet.extend_from_slice(text.as_bytes());
}

/// Reads one packet.
///
/// Args:
///     reader: The connection's reading half.
///     body: Scratch space, replaced with everything after the packet's length.
///
/// Returns:
///     The packet's first byte, or `None` when the connection has closed.
///
/// Raises:
///     std::io::Error: When reading fails or the length is malformed.
async fn read_packet(
    reader: &mut BufReader<OwnedReadHalf>,
    body: &mut Vec<u8>,
) -> std::io::Result<Option<u8>> {
    let first = match reader.read_u8().await {
        Ok(first) => first,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let byte = reader.read_u8().await?;
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return Err(std::io::Error::other("malformed MQTT remaining length"));
        }
    }
    body.resize(length, 0);
    reader.read_exact(body).await?;
    Ok(Some(first))
}

/// Splits a `PUBLISH` packet into its packet id and payload.
///
/// Args:
///     first: The packet's first byte, which carries the QoS.
///     body: Everything after the packet's length.
///
/// Returns:
///     The packet id, present for QoS 1 and 2, and the payload. `None` when the packet is cut
///     short.
pub fn publish_parts(first: u8, body: &[u8]) -> Option<(Option<u16>, &[u8])> {
    let topic_bytes = usize::from(u16::from_be_bytes([*body.first()?, *body.get(1)?]));
    let mut at = 2 + topic_bytes;
    let id = if (first >> 1) & 0x03 > 0 {
        let id = u16::from_be_bytes([*body.get(at)?, *body.get(at + 1)?]);
        at += 2;
        Some(id)
    } else {
        None
    };
    Some((id, body.get(at..)?))
}

/// Opens a connection with a clean session and no keep-alive.
///
/// Args:
///     address: The broker's `host:port`.
///
/// Returns:
///     The connection, once the broker has accepted it.
///
/// Raises:
///     String: When connecting fails or the broker refuses the connection.
pub async fn connect(address: &str) -> Result<MqttConn, String> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("connecting to MQTT at {address} failed: {e}"))?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let (read, write) = stream.into_split();
    let mut conn = MqttConn {
        reader: BufReader::with_capacity(BUFFER_BYTES, read),
        writer: BufWriter::with_capacity(BUFFER_BYTES, write),
    };
    let client = format!("bench-{}", NEXT_CLIENT.fetch_add(1, Ordering::Relaxed));
    let mut variable = Vec::new();
    push_string(&mut variable, "MQTT");
    variable.extend_from_slice(&[0x04, 0x02, 0x00, 0x00]);
    push_string(&mut variable, &client);
    let mut packet = vec![0x10];
    push_remaining_length(&mut packet, variable.len());
    packet.extend_from_slice(&variable);
    conn.send(&packet).await?;
    let mut body = Vec::new();
    match read_packet(&mut conn.reader, &mut body).await {
        Ok(Some(0x20)) if body.get(1) == Some(&0) => Ok(conn),
        Ok(Some(_)) => Err(format!("the MQTT broker refused the connection: {body:?}")),
        Ok(None) => Err("the MQTT broker closed the connection".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

impl MqttConn {
    /// Writes bytes and flushes them.
    ///
    /// Args:
    ///     bytes: One or more whole packets.
    ///
    /// Raises:
    ///     String: When the connection fails.
    async fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.writer
            .write_all(bytes)
            .await
            .map_err(|e| e.to_string())?;
        self.writer.flush().await.map_err(|e| e.to_string())
    }
}

/// Subscribes a connection to one topic and waits for the broker to grant it.
///
/// Args:
///     conn: An open connection.
///     topic: The topic filter.
///     qos: The most the subscriber wants: 0 or 1.
///
/// Raises:
///     String: When the broker refuses the subscription or the connection fails.
pub async fn subscribe_to(conn: &mut MqttConn, topic: &str, qos: u8) -> Result<(), String> {
    let mut variable = vec![0x00, 0x01];
    push_string(&mut variable, topic);
    variable.push(qos);
    let mut packet = vec![0x82];
    push_remaining_length(&mut packet, variable.len());
    packet.extend_from_slice(&variable);
    conn.send(&packet).await?;
    let mut body = Vec::new();
    match read_packet(&mut conn.reader, &mut body).await {
        Ok(Some(0x90)) if body.get(2).is_some_and(|granted| *granted <= 2) => Ok(()),
        Ok(Some(_)) => Err(format!(
            "the MQTT broker refused the subscription: {body:?}"
        )),
        Ok(None) => Err("the MQTT broker closed the connection".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// The writing half of a publisher's connection, with each topic already length-prefixed.
struct MqttOutlet {
    writer: BufWriter<OwnedWriteHalf>,
    topics: Vec<Vec<u8>>,
    padding: Vec<u8>,
    packet: Vec<u8>,
}

impl Outlet for MqttOutlet {
    /// Queues one `PUBLISH`, at QoS 1 when it carries an id and QoS 0 otherwise.
    ///
    /// Args:
    ///     topic: Which topic to publish to.
    ///     stamp_ns: The send time to write into the payload.
    ///     id: The packet id for a confirmed publish, between 1 and 65,535.
    ///
    /// Returns:
    ///     False when the connection has failed.
    async fn feed(&mut self, topic: usize, stamp_ns: u64, id: Option<u64>) -> bool {
        let Some(name) = self.topics.get(topic) else {
            return false;
        };
        let stamp = stamp(stamp_ns);
        let id_bytes = if id.is_some() { 2 } else { 0 };
        self.packet.clear();
        self.packet.push(if id.is_some() { 0x32 } else { 0x30 });
        push_remaining_length(
            &mut self.packet,
            name.len() + id_bytes + stamp.len() + self.padding.len(),
        );
        self.packet.extend_from_slice(name);
        if let Some(id) = id {
            self.packet.extend_from_slice(&(id as u16).to_be_bytes());
        }
        self.packet.extend_from_slice(stamp.as_bytes());
        self.packet.extend_from_slice(&self.padding);
        self.writer.write_all(&self.packet).await.is_ok()
    }

    /// Flushes the connection.
    ///
    /// Returns:
    ///     False when the connection has failed.
    async fn flush(&mut self) -> bool {
        self.writer.flush().await.is_ok()
    }
}

/// Publishes to an MQTT broker until the window ends.
///
/// Args:
///     conn: An open connection.
///     spec: The topics, pacing and payload size.
///     clock: The run's clock.
///     window: The measurement window.
///     totals: The group's totals.
///
/// Returns:
///     The stopped publisher, to be kept until the run has drained.
pub async fn publish(
    conn: MqttConn,
    spec: PublisherSpec,
    clock: Clock,
    window: Window,
    totals: Arc<Totals>,
) -> Publisher {
    let MqttConn { reader, writer } = conn;
    let replies = Replies::new(spec.mode);
    let mut outlet = MqttOutlet {
        writer,
        topics: spec
            .topics
            .iter()
            .map(|topic| {
                let mut name = Vec::new();
                push_string(&mut name, topic);
                name
            })
            .collect(),
        padding: padding(spec.payload_bytes).into_bytes(),
        packet: Vec::new(),
    };
    let reader = tokio::spawn(read_replies(reader, clock, window, replies.clone()));
    let sent = pace(&mut outlet, &spec, clock, window, &totals, &replies).await;
    Publisher::new(sent, replies, spec.mode, reader, outlet.writer)
}

/// Reads a publisher's connection until it closes, counting the broker's `PUBACK`s.
///
/// Args:
///     reader: The reading half of the publisher's connection.
///     clock: The run's clock.
///     window: The measurement window.
///     replies: Where acknowledgements are counted.
async fn read_replies(
    mut reader: BufReader<OwnedReadHalf>,
    clock: Clock,
    window: Window,
    replies: Replies,
) {
    let mut body = Vec::new();
    while let Ok(Some(first)) = read_packet(&mut reader, &mut body).await {
        if first >> 4 == PUBACK {
            replies.confirmed(clock, window);
        }
    }
}

/// Reads deliveries on a subscribed connection until told to stop.
///
/// A QoS 1 delivery is answered with a `PUBACK`. Acknowledgements are flushed when nothing
/// more is waiting in the read buffer, so a burst of deliveries is answered in one write.
///
/// Args:
///     conn: A connection that is already subscribed.
///     spec: Whether to record latency or read slowly.
///     clock: The run's clock.
///     window: The measurement window.
///     totals: The group's totals, when this subscriber counts towards them.
///     stop: Becomes true when the run has drained.
///
/// Returns:
///     What the subscriber counted.
pub async fn subscribe(
    conn: MqttConn,
    spec: SubscriberSpec,
    clock: Clock,
    window: Window,
    totals: Option<Arc<Totals>>,
    mut stop: watch::Receiver<bool>,
) -> SubscriberStats {
    let MqttConn {
        mut reader,
        mut writer,
    } = conn;
    let mut stats = SubscriberStats::new(&spec);
    let mut body = Vec::new();
    let mut unflushed = false;
    loop {
        let packet = tokio::select! {
            biased;
            _ = stop.changed() => break,
            packet = read_packet(&mut reader, &mut body) => packet,
        };
        let Ok(Some(first)) = packet else {
            break;
        };
        if first >> 4 == PUBLISH {
            match publish_parts(first, &body) {
                Some((id, payload)) => {
                    stats.delivery(payload, clock, window, totals.as_deref());
                    if let Some(id) = id {
                        let [high, low] = id.to_be_bytes();
                        if writer.write_all(&[0x40, 0x02, high, low]).await.is_err() {
                            break;
                        }
                        unflushed = true;
                    }
                    if let Some(delay) = spec.delay {
                        sleep(delay).await;
                    }
                }
                None => stats.errors += 1,
            }
        }
        if unflushed && reader.buffer().is_empty() {
            if writer.flush().await.is_err() {
                break;
            }
            unflushed = false;
        }
    }
    stats
}
