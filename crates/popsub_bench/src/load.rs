//! The traffic every broker gets: pacing for publishers and counting for subscribers.
//!
//! This module knows nothing about a wire protocol. A protocol module (`popsub`, `nats`,
//! `mqtt`) supplies an `Outlet` that writes one publish, and calls `SubscriberStats::delivery`
//! for each message it reads, so every broker is paced and measured by the same code.
//!
//! Every payload starts with the time it was sent, as 16 hex digits of nanoseconds on the
//! run's `Clock`, so a subscriber in the same process can work out how long delivery took.
//! Throughput is counted by when a message was sent or arrived, inside the measurement
//! `Window`.

use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hdrhistogram::Histogram;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};

/// How many publishes a flat-out publisher queues before flushing its connection.
const FLAT_OUT_BATCH: usize = 64;

/// How far behind schedule a fixed-rate publish may be before it is stamped with the time it
/// was due instead of the time it was sent, so a stalled connection shows up as latency.
const LATE_NS: u64 = 5_000_000;

/// The longest delivery time the latency histogram tracks, in microseconds (two minutes).
const MAX_LATENCY_MICROS: u64 = 120_000_000;

/// How many hex digits of each payload hold the send time.
pub const STAMP_DIGITS: usize = 16;

/// The run's clock: nanoseconds since the benchmark started.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    start: Instant,
}

impl Clock {
    /// Starts the clock.
    ///
    /// Returns:
    ///     A clock reading zero now.
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Reads the clock.
    ///
    /// Returns:
    ///     Nanoseconds since `start`.
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Converts a clock reading into the Tokio instant it names.
    ///
    /// Args:
    ///     at_ns: A reading from `now_ns`, possibly in the future.
    ///
    /// Returns:
    ///     The instant, for `sleep_until`.
    pub fn instant(&self, at_ns: u64) -> Instant {
        self.start + Duration::from_nanos(at_ns)
    }
}

/// The part of a run that is measured, after warm-up.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// When measuring starts, on the run's clock.
    pub begin_ns: u64,
    /// When measuring stops and publishers stop sending.
    pub end_ns: u64,
}

impl Window {
    /// Whether a clock reading falls inside the window.
    ///
    /// Args:
    ///     at_ns: A reading from `Clock::now_ns`.
    ///
    /// Returns:
    ///     True from `begin_ns` up to, but not including, `end_ns`.
    pub fn contains(&self, at_ns: u64) -> bool {
        at_ns >= self.begin_ns && at_ns < self.end_ns
    }

    /// The window's length.
    ///
    /// Returns:
    ///     Seconds between `begin_ns` and `end_ns`.
    pub fn seconds(&self) -> f64 {
        (self.end_ns - self.begin_ns) as f64 / 1e9
    }
}

/// How a publisher paces itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Unconfirmed publishes as fast as the connection takes them, bounded by TCP backpressure.
    FlatOut,
    /// Unconfirmed publishes at a fixed number of messages a second, whatever the server does.
    Rate(u64),
    /// Publishes the broker confirms, with at most `window` waiting for their confirmation.
    /// Subscribers acknowledge every message where the protocol has acknowledgements.
    Confirmed {
        /// The most publishes in flight at once.
        window: usize,
    },
}

impl Mode {
    /// Describes the pacing in a few words, for the report.
    ///
    /// Returns:
    ///     Text such as `QoS 0, flat out`.
    pub fn label(self) -> String {
        match self {
            Mode::FlatOut => "QoS 0, flat out".to_string(),
            Mode::Rate(rate) => format!("QoS 0, {rate}/s"),
            Mode::Confirmed { window } => format!("QoS 1, {window} in flight"),
        }
    }

    /// How many publishes may wait for a confirmation at once.
    ///
    /// Returns:
    ///     The window for confirmed publishes, zero otherwise.
    pub fn in_flight(self) -> usize {
        match self {
            Mode::Confirmed { window } => window,
            Mode::FlatOut | Mode::Rate(_) => 0,
        }
    }
}

/// Totals for one group of topics over the whole run, warm-up included, used to check nothing
/// was lost.
#[derive(Debug, Default)]
pub struct Totals {
    /// Publishes written to a connection.
    pub sent: AtomicU64,
    /// Deliveries read by all of the group's counted subscribers together.
    pub received: AtomicU64,
}

/// Writes a send time as the first `STAMP_DIGITS` bytes of a payload.
///
/// Args:
///     stamp_ns: The send time on the run's clock.
///
/// Returns:
///     Sixteen lowercase hex digits.
pub fn stamp(stamp_ns: u64) -> String {
    format!("{stamp_ns:016x}")
}

/// Builds the bytes that follow the stamp in every payload.
///
/// Args:
///     payload_bytes: The payload's whole length. Anything under `STAMP_DIGITS` is raised to it.
///
/// Returns:
///     Filler that brings a stamped payload up to `payload_bytes`.
pub fn padding(payload_bytes: usize) -> String {
    "x".repeat(payload_bytes.saturating_sub(STAMP_DIGITS))
}

/// Reads the send time back out of a payload.
///
/// Args:
///     payload: A payload that starts with a `stamp`.
///
/// Returns:
///     The send time on the run's clock, or `None` for any other payload.
pub fn stamp_of(payload: &[u8]) -> Option<u64> {
    let digits = std::str::from_utf8(payload.get(..STAMP_DIGITS)?).ok()?;
    u64::from_str_radix(digits, 16).ok()
}

/// What one publisher is asked to do.
#[derive(Debug, Clone)]
pub struct PublisherSpec {
    /// The topics to publish to, one after the other. A publisher with several roams over them.
    pub topics: Vec<String>,
    /// How to pace the publishes.
    pub mode: Mode,
    /// The payload's length in bytes.
    pub payload_bytes: usize,
}

/// Where a publisher's publishes go: one connection to a broker, in that broker's protocol.
pub trait Outlet: Send {
    /// Queues one publish without necessarily sending it.
    ///
    /// Args:
    ///     topic: Which of the publisher's topics to publish to, as an index.
    ///     stamp_ns: The send time to put at the start of the payload.
    ///     id: An id for publishes the broker confirms, unique among those in flight.
    ///
    /// Returns:
    ///     False when the connection has failed and the publisher should stop.
    fn feed(
        &mut self,
        topic: usize,
        stamp_ns: u64,
        id: Option<u64>,
    ) -> impl Future<Output = bool> + Send;

    /// Sends everything queued.
    ///
    /// Returns:
    ///     False when the connection has failed and the publisher should stop.
    fn flush(&mut self) -> impl Future<Output = bool> + Send;
}

/// What a publisher's reading task counts, shared with its sending side.
#[derive(Debug, Clone)]
pub struct Replies {
    permits: Arc<Semaphore>,
    confirmed_in_window: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
}

impl Replies {
    /// Starts counting for one publisher.
    ///
    /// Args:
    ///     mode: The publisher's pacing, for how many publishes may be in flight.
    ///
    /// Returns:
    ///     Counters at zero, with one permit per publish allowed in flight.
    pub fn new(mode: Mode) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(mode.in_flight())),
            confirmed_in_window: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Records that the broker confirmed one publish, which lets the publisher send another.
    ///
    /// Args:
    ///     clock: The run's clock.
    ///     window: The measurement window.
    pub fn confirmed(&self, clock: Clock, window: Window) {
        if window.contains(clock.now_ns()) {
            self.confirmed_in_window.fetch_add(1, Ordering::Relaxed);
        }
        self.permits.add_permits(1);
    }

    /// Records that the broker reported an error to the publisher.
    pub fn error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// A publisher that has stopped sending, with its connection still open.
///
/// The connection stays open so the broker can finish reading what was sent. Dropping this
/// closes it and stops the task that reads replies.
pub struct Publisher {
    sent_in_window: u64,
    replies: Replies,
    confirmed: bool,
    reader: JoinHandle<()>,
    _connection: Box<dyn Any + Send>,
}

impl Publisher {
    /// Wraps up a publisher that has finished sending.
    ///
    /// Args:
    ///     sent_in_window: What `pace` returned.
    ///     replies: The counters its reading task updates.
    ///     mode: The publisher's pacing.
    ///     reader: The task reading its replies.
    ///     connection: Whatever keeps the connection open, such as its writing half.
    ///
    /// Returns:
    ///     The stopped publisher, to be kept until the run has drained.
    pub fn new(
        sent_in_window: u64,
        replies: Replies,
        mode: Mode,
        reader: JoinHandle<()>,
        connection: impl Any + Send,
    ) -> Self {
        Self {
            sent_in_window,
            replies,
            confirmed: mode.in_flight() > 0,
            reader,
            _connection: Box::new(connection),
        }
    }

    /// How many publishes count towards the measured rate.
    ///
    /// Returns:
    ///     For confirmed publishes, the confirmations that arrived inside the window.
    ///     Otherwise the publishes sent inside it.
    pub fn published_in_window(&self) -> u64 {
        if self.confirmed {
            self.replies.confirmed_in_window.load(Ordering::Relaxed)
        } else {
            self.sent_in_window
        }
    }

    /// How many errors the broker reported to this publisher.
    ///
    /// Returns:
    ///     The count over the whole run.
    pub fn errors(&self) -> u64 {
        self.replies.errors.load(Ordering::Relaxed)
    }
}

impl Drop for Publisher {
    /// Stops the task that reads this publisher's replies.
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Publishes through an outlet until the window ends, paced as the spec says.
///
/// Sending starts at once, so the time before `window.begin_ns` is warm-up.
///
/// Args:
///     outlet: The connection to publish through.
///     spec: The topics and pacing.
///     clock: The run's clock.
///     window: The measurement window. Sending stops at its end.
///     totals: The group's totals, where every publish written is counted.
///     replies: The publisher's reply counters, for the in-flight limit of confirmed publishes.
///
/// Returns:
///     How many publishes were sent inside the window.
pub async fn pace<O: Outlet>(
    outlet: &mut O,
    spec: &PublisherSpec,
    clock: Clock,
    window: Window,
    totals: &Totals,
    replies: &Replies,
) -> u64 {
    let mut sender = Sender {
        outlet,
        topics: spec.topics.len().max(1),
        totals,
        window,
        sent_in_window: 0,
        fed: 0,
    };
    match spec.mode {
        Mode::FlatOut => sender.flat_out(clock).await,
        Mode::Rate(rate) => sender.at_rate(clock, rate).await,
        Mode::Confirmed { .. } => sender.confirmed(clock, &replies.permits).await,
    }
    sender.sent_in_window
}

/// The sending side of a publisher: its outlet and the counters it updates.
struct Sender<'a, O: Outlet> {
    outlet: &'a mut O,
    topics: usize,
    totals: &'a Totals,
    window: Window,
    sent_in_window: u64,
    fed: usize,
}

impl<O: Outlet> Sender<'_, O> {
    /// Queues one publish, to the next topic in turn, and counts it.
    ///
    /// Args:
    ///     stamp_ns: The send time to write into the payload.
    ///     id: An id, for confirmed publishes.
    ///
    /// Returns:
    ///     False when the connection has failed and the publisher should stop.
    async fn feed(&mut self, stamp_ns: u64, id: Option<u64>) -> bool {
        if !self.outlet.feed(self.fed % self.topics, stamp_ns, id).await {
            return false;
        }
        self.fed += 1;
        self.totals.sent.fetch_add(1, Ordering::Relaxed);
        if self.window.contains(stamp_ns) {
            self.sent_in_window += 1;
        }
        true
    }

    /// Publishes as fast as the connection accepts, until the window ends.
    ///
    /// Args:
    ///     clock: The run's clock.
    async fn flat_out(&mut self, clock: Clock) {
        while clock.now_ns() < self.window.end_ns {
            for _ in 0..FLAT_OUT_BATCH {
                if !self.feed(clock.now_ns(), None).await {
                    return;
                }
            }
            if !self.outlet.flush().await {
                return;
            }
        }
    }

    /// Publishes at a fixed rate, until the window ends.
    ///
    /// A timer fires every millisecond and sends whatever is due. A publish is stamped with the
    /// time it was sent, unless it went out more than `LATE_NS` after it was due, in which case
    /// it is stamped with the time it was due.
    ///
    /// Args:
    ///     clock: The run's clock.
    ///     rate: Messages a second.
    async fn at_rate(&mut self, clock: Clock, rate: u64) {
        let period_ns = 1_000_000_000 / rate.max(1);
        let first = clock.now_ns();
        let mut sent: u64 = 0;
        let mut tick = interval(Duration::from_millis(1));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let now = clock.now_ns();
            if now >= self.window.end_ns {
                return;
            }
            let mut fed = false;
            while first + sent * period_ns <= now {
                let due = first + sent * period_ns;
                let stamp = if now - due > LATE_NS {
                    due
                } else {
                    clock.now_ns()
                };
                if !self.feed(stamp, None).await {
                    return;
                }
                sent += 1;
                fed = true;
            }
            if fed && !self.outlet.flush().await {
                return;
            }
        }
    }

    /// Publishes with a bounded number of unconfirmed publishes, until the window ends.
    ///
    /// Each publish takes a permit, and `Replies::confirmed` gives one back. Publishes are
    /// queued while permits are free and flushed when they run out. Ids count up from 1 and
    /// wrap before 65,536, which is as far as an MQTT packet id goes.
    ///
    /// Args:
    ///     clock: The run's clock.
    ///     permits: The publishes that may still be sent without waiting for a confirmation.
    async fn confirmed(&mut self, clock: Clock, permits: &Semaphore) {
        let end = clock.instant(self.window.end_ns);
        let mut id: u64 = 1;
        loop {
            tokio::select! {
                biased;
                () = sleep_until(end) => return,
                permit = permits.acquire() => match permit {
                    Ok(permit) => permit.forget(),
                    Err(_) => return,
                },
            }
            loop {
                if !self.feed(clock.now_ns(), Some(id)).await {
                    return;
                }
                id = id % 65_535 + 1;
                match permits.try_acquire() {
                    Ok(permit) => permit.forget(),
                    Err(_) => break,
                }
            }
            if !self.outlet.flush().await {
                return;
            }
        }
    }
}

/// What one subscriber is asked to do.
#[derive(Debug, Clone)]
pub struct SubscriberSpec {
    /// Whether to acknowledge every delivery, where the protocol has acknowledgements.
    pub acknowledge: bool,
    /// Whether to record how long each delivery took.
    pub record_latency: bool,
    /// A pause after every delivery, for a subscriber that is meant to fall behind.
    pub delay: Option<Duration>,
}

/// What one subscriber saw.
#[derive(Debug)]
pub struct SubscriberStats {
    /// Deliveries that arrived inside the window.
    pub in_window: u64,
    /// Delivery times in microseconds for those deliveries, when asked for.
    pub latency: Option<Histogram<u64>>,
    /// Errors the broker reported and frames that couldn't be read.
    pub errors: u64,
}

impl SubscriberStats {
    /// Starts counting for one subscriber.
    ///
    /// Args:
    ///     spec: Whether to record latency.
    ///
    /// Returns:
    ///     Counters at zero.
    pub fn new(spec: &SubscriberSpec) -> Self {
        Self {
            in_window: 0,
            latency: spec
                .record_latency
                .then(|| Histogram::new_with_bounds(1, MAX_LATENCY_MICROS, 2).ok())
                .flatten(),
            errors: 0,
        }
    }

    /// Counts one delivery.
    ///
    /// Args:
    ///     payload: The message's payload, which starts with its send time.
    ///     clock: The run's clock.
    ///     window: The measurement window.
    ///     totals: The group's totals, when this subscriber counts towards them.
    pub fn delivery(
        &mut self,
        payload: &[u8],
        clock: Clock,
        window: Window,
        totals: Option<&Totals>,
    ) {
        let now = clock.now_ns();
        if let Some(totals) = totals {
            totals.received.fetch_add(1, Ordering::Relaxed);
        }
        if window.contains(now) {
            self.in_window += 1;
            if let Some(latency) = &mut self.latency
                && let Some(sent) = stamp_of(payload)
            {
                latency.saturating_record(now.saturating_sub(sent) / 1_000);
            }
        }
    }
}
