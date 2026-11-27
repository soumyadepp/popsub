//! NATS's wire: the text protocol over TCP, for running the same loads against `nats-server`.
//!
//! Two setups are measured. Core NATS keeps nothing: a publish goes to whoever is subscribed.
//! With JetStream, a file-backed stream captures every subject under `bench.`, and a publish
//! that names a reply subject gets an acknowledgement once it is stored. Subscribers are plain
//! core subscribers in both, so delivery is the same path.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
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

/// The JetStream stream that stores every benchmark subject.
const STREAM: &str = "BENCH";

/// Numbers the reply subjects of confirmed publishers, so each has its own.
static NEXT_INBOX: AtomicU64 = AtomicU64::new(0);

/// One connection to `nats-server`.
pub struct NatsConn {
    reader: BufReader<OwnedReadHalf>,
    writer: BufWriter<OwnedWriteHalf>,
}

/// What a line from the server turned out to be.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    /// A message, whose payload has this many bytes.
    Message(usize),
    /// A keep-alive the client must answer.
    Ping,
    /// The answer to a client's `PING`.
    Pong,
    /// The server refusing something.
    Error,
    /// Anything else, such as `INFO` or `+OK`.
    Other,
}

/// Turns a PopSub-style topic into a NATS subject.
///
/// Args:
///     topic: A topic with `/` between levels.
///
/// Returns:
///     The same levels joined with `.`.
pub fn subject(topic: &str) -> String {
    topic.replace('/', ".")
}

/// Opens a connection and completes the handshake.
///
/// Args:
///     address: The server's `host:port`.
///
/// Returns:
///     The connection, once the server has answered a `PING`.
///
/// Raises:
///     String: When connecting fails or the server refuses the handshake.
pub async fn connect(address: &str) -> Result<NatsConn, String> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("connecting to NATS at {address} failed: {e}"))?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let (read, write) = stream.into_split();
    let mut conn = NatsConn {
        reader: BufReader::with_capacity(BUFFER_BYTES, read),
        writer: BufWriter::with_capacity(BUFFER_BYTES, write),
    };
    let hello = concat!(
        r#"CONNECT {"verbose":false,"pedantic":false,"tls_required":false,"#,
        r#""name":"popsub-bench","lang":"rust","version":"0","protocol":1}"#,
        "\r\nPING\r\n"
    );
    conn.send(hello.as_bytes()).await?;
    conn.await_pong().await?;
    Ok(conn)
}

impl NatsConn {
    /// Writes bytes and flushes them.
    ///
    /// Args:
    ///     bytes: Protocol text to send.
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

    /// Reads until the server answers a `PING`, which shows it handled everything before it.
    ///
    /// Raises:
    ///     String: When the server reports an error or the connection closes first.
    async fn await_pong(&mut self) -> Result<(), String> {
        let mut line = Vec::new();
        let mut payload = Vec::new();
        loop {
            match read_frame(&mut self.reader, &mut line, &mut payload).await {
                Ok(Some(Frame::Pong)) => return Ok(()),
                Ok(Some(Frame::Error)) => {
                    return Err(format!(
                        "NATS refused a request: {}",
                        String::from_utf8_lossy(&line).trim()
                    ));
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err("NATS closed the connection".to_string()),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

/// Reads one protocol line, and the payload that follows it when it is a message.
///
/// Args:
///     reader: The connection's reading half.
///     line: Scratch space for the line, replaced on every call.
///     payload: Scratch space for a message's payload and its trailing line ending.
///
/// Returns:
///     What the line was, or `None` when the connection has closed.
///
/// Raises:
///     std::io::Error: When reading fails.
async fn read_frame(
    reader: &mut BufReader<OwnedReadHalf>,
    line: &mut Vec<u8>,
    payload: &mut Vec<u8>,
) -> std::io::Result<Option<Frame>> {
    line.clear();
    if reader.read_until(b'\n', line).await? == 0 {
        return Ok(None);
    }
    if line.starts_with(b"MSG ") {
        let length = message_length(line).unwrap_or(0);
        payload.resize(length + 2, 0);
        reader.read_exact(payload).await?;
        return Ok(Some(Frame::Message(length)));
    }
    Ok(Some(if line.starts_with(b"PING") {
        Frame::Ping
    } else if line.starts_with(b"PONG") {
        Frame::Pong
    } else if line.starts_with(b"-ERR") {
        Frame::Error
    } else {
        Frame::Other
    }))
}

/// Reads the payload length from a `MSG` line, where it is the last field.
///
/// Args:
///     line: A line such as `MSG bench.a 1 128`, with or without its line ending.
///
/// Returns:
///     The length, or `None` when the last field isn't a number.
pub fn message_length(line: &[u8]) -> Option<usize> {
    std::str::from_utf8(line)
        .ok()?
        .split_whitespace()
        .next_back()?
        .parse()
        .ok()
}

/// Subscribes a connection to one topic and waits until the server has registered it.
///
/// Args:
///     conn: An open connection.
///     topic: The topic, in PopSub's `/` form.
///
/// Raises:
///     String: When the server refuses or the connection fails.
pub async fn subscribe_to(conn: &mut NatsConn, topic: &str) -> Result<(), String> {
    conn.send(format!("SUB {} 1\r\nPING\r\n", subject(topic)).as_bytes())
        .await?;
    conn.await_pong().await
}

/// Creates the file-backed JetStream stream that stores every subject under `bench.`.
///
/// The stream has no limits, so nothing is discarded during a run.
///
/// Args:
///     address: The server's `host:port`. It must have been started with JetStream on.
///
/// Raises:
///     String: When the server refuses to create the stream.
pub async fn create_stream(address: &str) -> Result<(), String> {
    let mut conn = connect(address).await?;
    let config = format!(
        r#"{{"name":"{STREAM}","subjects":["bench.>"],"retention":"limits","storage":"file","max_msgs":-1,"max_bytes":-1,"max_age":0,"num_replicas":1,"discard":"old"}}"#
    );
    let request = format!(
        "SUB _INBOX.setup 9\r\nPUB $JS.API.STREAM.CREATE.{STREAM} _INBOX.setup {}\r\n{config}\r\n",
        config.len()
    );
    conn.send(request.as_bytes()).await?;
    let mut line = Vec::new();
    let mut payload = Vec::new();
    loop {
        match read_frame(&mut conn.reader, &mut line, &mut payload).await {
            Ok(Some(Frame::Message(length))) => {
                let reply = String::from_utf8_lossy(&payload[..length]);
                return if reply.contains(r#""error""#) {
                    Err(format!("JetStream refused the stream: {reply}"))
                } else {
                    Ok(())
                };
            }
            Ok(Some(Frame::Error)) => {
                return Err(format!(
                    "NATS refused the stream request: {}",
                    String::from_utf8_lossy(&line).trim()
                ));
            }
            Ok(Some(_)) => {}
            Ok(None) => return Err("NATS closed the connection".to_string()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The writing half of a publisher's connection, with the start of a `PUB` line per topic.
struct NatsOutlet {
    writer: BufWriter<OwnedWriteHalf>,
    heads: Vec<Vec<u8>>,
    reply: Option<String>,
    padding: Vec<u8>,
    frame: Vec<u8>,
}

impl Outlet for NatsOutlet {
    /// Queues one `PUB`, naming the reply subject when the publish is to be confirmed.
    ///
    /// Args:
    ///     topic: Which topic to publish to.
    ///     stamp_ns: The send time to write into the payload.
    ///     id: Present for confirmed publishes. NATS has no use for its value.
    ///
    /// Returns:
    ///     False when the connection has failed.
    async fn feed(&mut self, topic: usize, stamp_ns: u64, id: Option<u64>) -> bool {
        let Some(head) = self.heads.get(topic) else {
            return false;
        };
        let stamp = stamp(stamp_ns);
        self.frame.clear();
        self.frame.extend_from_slice(head);
        if let (Some(_), Some(reply)) = (id, &self.reply) {
            self.frame.extend_from_slice(reply.as_bytes());
            self.frame.push(b' ');
        }
        self.frame
            .extend_from_slice((stamp.len() + self.padding.len()).to_string().as_bytes());
        self.frame.extend_from_slice(b"\r\n");
        self.frame.extend_from_slice(stamp.as_bytes());
        self.frame.extend_from_slice(&self.padding);
        self.frame.extend_from_slice(b"\r\n");
        self.writer.write_all(&self.frame).await.is_ok()
    }

    /// Flushes the connection.
    ///
    /// Returns:
    ///     False when the connection has failed.
    async fn flush(&mut self) -> bool {
        self.writer.flush().await.is_ok()
    }
}

/// Publishes to NATS until the window ends.
///
/// Confirmed publishes name a reply subject of the publisher's own, and each JetStream
/// acknowledgement arriving on it counts as one confirmation. They only make sense against a
/// server with the stream from `create_stream`.
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
    conn: NatsConn,
    spec: PublisherSpec,
    clock: Clock,
    window: Window,
    totals: Arc<Totals>,
) -> Publisher {
    let NatsConn { reader, mut writer } = conn;
    let replies = Replies::new(spec.mode);
    let reply = (spec.mode.in_flight() > 0).then(|| {
        format!(
            "_INBOX.bench.{}",
            NEXT_INBOX.fetch_add(1, Ordering::Relaxed)
        )
    });
    if let Some(reply) = &reply {
        let _ = writer
            .write_all(format!("SUB {reply} 1\r\n").as_bytes())
            .await;
        let _ = writer.flush().await;
    }
    let mut outlet = NatsOutlet {
        writer,
        heads: spec
            .topics
            .iter()
            .map(|topic| format!("PUB {} ", subject(topic)).into_bytes())
            .collect(),
        reply,
        padding: padding(spec.payload_bytes).into_bytes(),
        frame: Vec::new(),
    };
    let reader = tokio::spawn(read_replies(reader, clock, window, replies.clone()));
    let sent = pace(&mut outlet, &spec, clock, window, &totals, &replies).await;
    Publisher::new(sent, replies, spec.mode, reader, outlet.writer)
}

/// Reads a publisher's connection until it closes, counting JetStream acknowledgements.
///
/// Args:
///     reader: The reading half of the publisher's connection.
///     clock: The run's clock.
///     window: The measurement window.
///     replies: Where acknowledgements and errors are counted.
async fn read_replies(
    mut reader: BufReader<OwnedReadHalf>,
    clock: Clock,
    window: Window,
    replies: Replies,
) {
    let mut line = Vec::new();
    let mut payload = Vec::new();
    while let Ok(Some(frame)) = read_frame(&mut reader, &mut line, &mut payload).await {
        match frame {
            Frame::Message(length) => {
                if payload[..length].windows(7).any(|w| w == b"\"error\"") {
                    replies.error();
                }
                replies.confirmed(clock, window);
            }
            Frame::Error => replies.error(),
            Frame::Ping | Frame::Pong | Frame::Other => {}
        }
    }
}

/// Reads deliveries on a subscribed connection until told to stop.
///
/// Args:
///     conn: A connection that is already subscribed.
///     spec: Whether to record latency or read slowly. Core subscribers don't acknowledge.
///     clock: The run's clock.
///     window: The measurement window.
///     totals: The group's totals, when this subscriber counts towards them.
///     stop: Becomes true when the run has drained.
///
/// Returns:
///     What the subscriber counted.
pub async fn subscribe(
    conn: NatsConn,
    spec: SubscriberSpec,
    clock: Clock,
    window: Window,
    totals: Option<Arc<Totals>>,
    mut stop: watch::Receiver<bool>,
) -> SubscriberStats {
    let NatsConn {
        mut reader,
        mut writer,
    } = conn;
    let mut stats = SubscriberStats::new(&spec);
    let mut line = Vec::new();
    let mut payload = Vec::new();
    loop {
        let frame = tokio::select! {
            biased;
            _ = stop.changed() => break,
            frame = read_frame(&mut reader, &mut line, &mut payload) => frame,
        };
        match frame {
            Ok(Some(Frame::Message(length))) => {
                stats.delivery(&payload[..length], clock, window, totals.as_deref());
                if let Some(delay) = spec.delay {
                    sleep(delay).await;
                }
            }
            Ok(Some(Frame::Ping)) => {
                if writer.write_all(b"PONG\r\n").await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
            Ok(Some(Frame::Error)) => stats.errors += 1,
            Ok(Some(Frame::Pong | Frame::Other)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    stats
}
