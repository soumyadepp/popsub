//! The benchmark scenarios and the runners that measure one of them against a fresh server.
//!
//! A scenario is one or more groups of topics. Every topic in a group carries the same load,
//! and a group is reported as one row. Most scenarios apply a steady load for a fixed window
//! (`run_load`); the replay scenario stores messages first and then times reading them back
//! (`run_replay`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt, stream};
use hdrhistogram::Histogram;
use popsub_protocol::{Access, ClientFrame, ClientMessage, RequestId, ServerMessage, StartFrom};
use tokio::sync::watch;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;

use crate::conn::{BENCH_PASSWORD, Socket, create_account, login, open, request, subscribe};
use crate::load::{
    Clock, Mode, Publisher, PublisherSpec, SubscriberSpec, SubscriberStats, Totals, Window,
};
use crate::mqtt::{self, MqttConn};
use crate::nats::{self, NatsConn};
use crate::popsub::{self, PublishTemplate, delivery_seq};
use crate::report::thousands;
use crate::server::{Backend, ServerProcess, cpu_seconds, resident_bytes};

/// How many connections are opened at once during setup, kept under the usual listen backlog.
const CONNECT_AT_ONCE: usize = 64;

/// How often the drain checks whether every delivery has arrived.
const DRAIN_POLL: Duration = Duration::from_millis(100);

/// How long the drain waits with no new deliveries before giving up.
const DRAIN_QUIET: Duration = Duration::from_secs(2);

/// The longest the drain waits in total.
const DRAIN_LIMIT: Duration = Duration::from_secs(20);

/// The in-flight limit confirmed publishers use, below the server's per-subscriber window of 100.
const CONFIRM_WINDOW: usize = 64;

/// The fixed rate of the quiet topic whose latency the isolation scenarios watch.
const QUIET_RATE: u64 = 200;

/// How long a slow subscriber pauses after each delivery, which caps it near 500 a second.
const SLOW_DELAY: Duration = Duration::from_millis(2);

/// The longest a replay may take before the scenario is abandoned.
const REPLAY_LIMIT: Duration = Duration::from_secs(120);

/// How much resident memory a broker may use before a scenario is stopped, in mebibytes.
///
/// A flat-out fan-out can make a broker buffer without bound for subscribers that can't keep
/// up. Stopping there keeps a run from exhausting the machine.
const MEMORY_LIMIT_MIB: f64 = 6.0 * 1024.0;

/// How often the broker's memory is checked while a scenario runs.
const MEMORY_POLL: Duration = Duration::from_millis(250);

/// Connection counts from which the setup rate is worth reporting.
const MANY_CONNECTIONS: usize = 1000;

/// Retention settings that keep every message for the length of a run, so trimming doesn't
/// take part in what a scenario measures.
const KEEP_EVERYTHING: [(&str, &str); 3] = [
    ("STORAGE__RETENTION__MAX_MESSAGES", "1000000000"),
    ("STORAGE__RETENTION__MAX_BYTES", "1000000000000"),
    ("STORAGE__RETENTION__MAX_AGE_SECS", "86400"),
];

/// The broker a run measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    /// PopSub, over its WebSocket protocol.
    Popsub,
    /// Core NATS: nothing is stored.
    Nats,
    /// NATS with a file-backed JetStream stream storing every message.
    NatsJetstream,
    /// Mosquitto, over MQTT 3.1.1, with persistence off.
    Mosquitto,
}

impl Target {
    /// The target's name in reports.
    ///
    /// Returns:
    ///     A short name such as `NATS JetStream`.
    pub fn label(self) -> &'static str {
        match self {
            Target::Popsub => "PopSub",
            Target::Nats => "NATS core",
            Target::NatsJetstream => "NATS JetStream",
            Target::Mosquitto => "Mosquitto",
        }
    }

    /// Whether a scenario can be run against this target.
    ///
    /// PopSub runs everything. The others run steady loads that use nothing PopSub-specific:
    /// no server settings, no wildcard, shared or slow subscribers, and no roaming publishers.
    /// Core NATS has no confirmed publishes, so it skips those too.
    ///
    /// Args:
    ///     scenario: The scenario to check.
    ///
    /// Returns:
    ///     True when the scenario means the same thing on this target.
    pub fn supports(self, scenario: &Scenario) -> bool {
        if self == Target::Popsub {
            return true;
        }
        scenario.kind == Kind::Load
            && scenario.settings.is_empty()
            && !scenario.default_retention
            && scenario.groups.iter().all(|group| {
                !group.shared
                    && group.pattern_subscribers == 0
                    && group.slow_subscribers == 0
                    && matches!(group.publishers, Publishers::PerTopic(_))
                    && (self != Target::Nats || group.mode.in_flight() == 0)
            })
    }
}

/// One open connection to whichever broker is being measured.
enum Link {
    /// A PopSub WebSocket, boxed because it is much larger than the other connections.
    Popsub(Box<Socket>),
    /// A NATS connection.
    Nats(NatsConn),
    /// An MQTT connection.
    Mqtt(MqttConn),
}

/// Where a group's publishers send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publishers {
    /// This many publishers on every topic, each with its own connection and account.
    PerTopic(usize),
    /// This many publishers in total, each sending to all the group's topics in turn.
    Roaming(usize),
}

/// A set of topics that carry the same load and are reported together.
#[derive(Debug, Clone)]
pub struct Group {
    /// The row's label within its scenario.
    pub label: &'static str,
    /// How many topics the group has.
    pub topics: usize,
    /// The group's publishers.
    pub publishers: Publishers,
    /// Subscribers on each topic, each on its own connection.
    pub subscribers: usize,
    /// Whether each topic's subscribers form one shared subscription, so each message goes to
    /// one of them.
    pub shared: bool,
    /// Subscribers to a wildcard pattern covering all the group's topics.
    pub pattern_subscribers: usize,
    /// Extra subscribers on each topic that pause after every delivery and aren't counted.
    pub slow_subscribers: usize,
    /// How each publisher paces itself.
    pub mode: Mode,
    /// A payload size for this group, instead of the run's.
    pub payload_bytes: Option<usize>,
}

impl Group {
    /// A group with one publisher on each topic.
    ///
    /// Args:
    ///     label: The row's label.
    ///     topics: How many topics.
    ///     subscribers: Subscribers on each topic.
    ///     mode: How the publishers pace themselves.
    ///
    /// Returns:
    ///     The group.
    fn new(label: &'static str, topics: usize, subscribers: usize, mode: Mode) -> Self {
        Self {
            label,
            topics,
            publishers: Publishers::PerTopic(1),
            subscribers,
            shared: false,
            pattern_subscribers: 0,
            slow_subscribers: 0,
            mode,
            payload_bytes: None,
        }
    }

    /// Sets the group's publishers.
    ///
    /// Args:
    ///     publishers: Where the publishers send and how many there are.
    ///
    /// Returns:
    ///     The group.
    fn with_publishers(mut self, publishers: Publishers) -> Self {
        self.publishers = publishers;
        self
    }

    /// Makes each topic's subscribers one shared subscription.
    ///
    /// Returns:
    ///     The group.
    fn sharing(mut self) -> Self {
        self.shared = true;
        self
    }

    /// Adds subscribers to a wildcard pattern over the group's topics.
    ///
    /// Args:
    ///     count: How many.
    ///
    /// Returns:
    ///     The group.
    fn with_patterns(mut self, count: usize) -> Self {
        self.pattern_subscribers = count;
        self
    }

    /// Adds slow, uncounted subscribers to each topic.
    ///
    /// Args:
    ///     count: How many on each topic.
    ///
    /// Returns:
    ///     The group.
    fn with_slow(mut self, count: usize) -> Self {
        self.slow_subscribers = count;
        self
    }

    /// Sets the group's payload size.
    ///
    /// Args:
    ///     bytes: The payload's length.
    ///
    /// Returns:
    ///     The group.
    fn with_payload(mut self, bytes: usize) -> Self {
        self.payload_bytes = Some(bytes);
        self
    }

    /// How many counted deliveries one publish to the group causes.
    ///
    /// Returns:
    ///     One per topic subscriber, or one in total when they share, plus one per wildcard
    ///     subscriber. Slow subscribers aren't counted.
    pub fn deliveries_per_publish(&self) -> u64 {
        let on_topic = if self.shared {
            self.subscribers.min(1)
        } else {
            self.subscribers
        };
        (on_topic + self.pattern_subscribers) as u64
    }

    /// Describes the group's shape in words, for the report.
    ///
    /// Returns:
    ///     Text such as `8 topics, 1 publisher each, 1 subscriber each`.
    pub fn describe(&self) -> String {
        let each = if self.topics > 1 { " each" } else { "" };
        let mut parts = vec![plural(self.topics, "topic")];
        match self.publishers {
            Publishers::PerTopic(0) => parts.push("no publisher".to_string()),
            Publishers::PerTopic(count) => {
                parts.push(format!("{}{each}", plural(count, "publisher")));
            }
            Publishers::Roaming(count) => {
                parts.push(format!("{} roaming", plural(count, "publisher")));
            }
        }
        if self.subscribers > 0 {
            let sharing = if self.shared { " sharing" } else { "" };
            parts.push(format!(
                "{}{sharing}{each}",
                plural(self.subscribers, "subscriber")
            ));
        }
        if self.pattern_subscribers > 0 {
            parts.push(plural(self.pattern_subscribers, "wildcard subscriber"));
        }
        if self.slow_subscribers > 0 {
            parts.push(format!(
                "{}{each}",
                plural(self.slow_subscribers, "slow subscriber")
            ));
        }
        parts.join(", ")
    }
}

/// Writes a count with its noun, plural when the count isn't one.
///
/// Args:
///     count: The count.
///     noun: The singular noun.
///
/// Returns:
///     Text such as `1 topic` or `1,000 topics`.
fn plural(count: usize, noun: &str) -> String {
    let s = if count == 1 { "" } else { "s" };
    format!("{} {noun}{s}", thousands(count as f64))
}

/// How a scenario is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A steady load for the run's window.
    Load,
    /// Store this many messages with no subscriber, restart the server, then time one
    /// subscriber reading them all from the first.
    Replay {
        /// How many messages to store.
        messages: u64,
    },
}

/// One benchmark: a name, what it shows, and the load to apply.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// The name used with `--scenario`.
    pub name: &'static str,
    /// One line on what the scenario is for.
    pub about: &'static str,
    /// How the scenario is measured.
    pub kind: Kind,
    /// The groups of topics to load at the same time.
    pub groups: Vec<Group>,
    /// Server settings for this scenario only, as environment variables.
    pub settings: Vec<(&'static str, &'static str)>,
    /// Whether the server keeps its default retention, instead of `KEEP_EVERYTHING`.
    pub default_retention: bool,
}

impl Scenario {
    /// A steady-load scenario with the usual server settings.
    ///
    /// Args:
    ///     name: The name used with `--scenario`.
    ///     about: One line on what it is for.
    ///     groups: The groups to load.
    ///
    /// Returns:
    ///     The scenario.
    fn load(name: &'static str, about: &'static str, groups: Vec<Group>) -> Self {
        Self {
            name,
            about,
            kind: Kind::Load,
            groups,
            settings: Vec::new(),
            default_retention: false,
        }
    }

    /// Adds server settings for this scenario.
    ///
    /// Args:
    ///     settings: Environment variables for the server.
    ///
    /// Returns:
    ///     The scenario.
    fn with_settings(mut self, settings: &[(&'static str, &'static str)]) -> Self {
        self.settings.extend_from_slice(settings);
        self
    }

    /// Leaves the server's retention at its defaults.
    ///
    /// Returns:
    ///     The scenario.
    fn with_default_retention(mut self) -> Self {
        self.default_retention = true;
        self
    }

    /// The settings the server is started with for this scenario.
    ///
    /// Returns:
    ///     `KEEP_EVERYTHING` unless the scenario keeps the default retention, followed by the
    ///     scenario's own settings.
    fn server_settings(&self) -> Vec<(&'static str, &'static str)> {
        let mut settings = Vec::new();
        if !self.default_retention {
            settings.extend_from_slice(&KEEP_EVERYTHING);
        }
        settings.extend_from_slice(&self.settings);
        settings
    }
}

/// Delivery time percentiles for one group, from publish to arrival at the subscriber.
#[derive(Debug, Clone, Copy)]
pub struct Latency {
    /// The median, in milliseconds.
    pub p50_ms: f64,
    /// The 99th percentile, in milliseconds.
    pub p99_ms: f64,
    /// The slowest delivery, in milliseconds.
    pub max_ms: f64,
}

/// What was measured for one group.
#[derive(Debug, Clone)]
pub struct GroupResult {
    /// The group's label within its scenario.
    pub label: &'static str,
    /// The group's shape in words.
    pub load: String,
    /// How the group's publishers were paced.
    pub mode: String,
    /// Publishes a second across the group, inside the window.
    pub publishes_per_sec: f64,
    /// Deliveries a second across all the group's counted subscribers, inside the window.
    pub deliveries_per_sec: f64,
    /// Delivery times, for groups that don't publish flat out.
    pub latency: Option<Latency>,
    /// `error` frames and unreadable frames seen by the group's connections.
    pub errors: u64,
    /// Whether every publish reached every counted subscriber by the end of the drain.
    pub complete: bool,
}

/// What was measured for one scenario.
#[derive(Debug, Clone)]
pub struct ScenarioResult {
    /// The scenario's name.
    pub name: &'static str,
    /// One row per group.
    pub groups: Vec<GroupResult>,
    /// CPU cores the server used on average while measuring.
    pub server_cores: Option<f64>,
    /// CPU cores the load generator used on average while measuring.
    pub loadgen_cores: Option<f64>,
    /// The server's resident memory at the end of measuring, in mebibytes.
    pub server_rss_mib: Option<f64>,
    /// Other facts worth reporting, one sentence each.
    pub notes: Vec<String>,
}

/// Settings shared by every scenario in a run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The broker to measure.
    pub target: Target,
    /// Path to the broker's executable: `popsub`, `nats-server` or `mosquitto`.
    pub binary: PathBuf,
    /// The storage backend the server runs with.
    pub backend: Backend,
    /// Traffic time before measuring starts.
    pub warmup: Duration,
    /// How long the measurement window is.
    pub duration: Duration,
    /// The payload's length in bytes.
    pub payload_bytes: usize,
}

/// The scenarios, in the order they run.
///
/// Returns:
///     Every scenario the benchmark knows.
pub fn scenarios() -> Vec<Scenario> {
    let confirmed = Mode::Confirmed {
        window: CONFIRM_WINDOW,
    };
    let flat = Mode::FlatOut;
    let quiet = || Group::new("quiet", 1, 1, Mode::Rate(QUIET_RATE));
    let one = |mode| vec![Group::new("all", 1, 1, mode)];
    vec![
        Scenario::load(
            "one-topic",
            "One publisher and one subscriber, QoS 0, flat out",
            one(flat),
        ),
        Scenario::load(
            "one-topic-qos1",
            "One publisher and one subscriber, QoS 1 with acknowledgements",
            one(confirmed),
        ),
        Scenario::load(
            "one-topic-default-retention",
            "one-topic with the default retention of 1,000 messages a topic",
            one(flat),
        )
        .with_default_retention(),
        Scenario::load(
            "one-topic-memory",
            "one-topic on the memory backend",
            one(flat),
        )
        .with_settings(&[("STORAGE_BACKEND", "memory")]),
        Scenario::load(
            "one-topic-qos1-synced",
            "one-topic-qos1 with every publish flushed to disk before it is confirmed",
            one(confirmed),
        )
        .with_settings(&[
            ("STORAGE_BACKEND", "file"),
            ("STORAGE_DURABILITY", "synced"),
        ]),
        Scenario::load(
            "payload-1k",
            "one-topic with 1 KiB payloads",
            vec![Group::new("all", 1, 1, flat).with_payload(1024)],
        ),
        Scenario::load(
            "payload-16k",
            "one-topic with 16 KiB payloads",
            vec![Group::new("all", 1, 1, flat).with_payload(16 * 1024)],
        ),
        Scenario::load(
            "fanin-8",
            "8 publishers to one topic with one subscriber, flat out",
            vec![Group::new("all", 1, 1, flat).with_publishers(Publishers::PerTopic(8))],
        ),
        Scenario::load(
            "fanout-10",
            "One publisher to 10 subscribers on one topic, flat out",
            vec![Group::new("all", 1, 10, flat)],
        ),
        Scenario::load(
            "fanout-100",
            "One publisher to 100 subscribers on one topic, flat out",
            vec![Group::new("all", 1, 100, flat)],
        ),
        Scenario::load(
            "fanout-1000",
            "One publisher to 1,000 subscribers on one topic, flat out",
            vec![Group::new("all", 1, 1000, flat)],
        ),
        Scenario::load(
            "topics-2",
            "2 independent topics, one publisher and one subscriber each, flat out",
            vec![Group::new("all", 2, 1, flat)],
        ),
        Scenario::load(
            "topics-4",
            "4 independent topics, flat out",
            vec![Group::new("all", 4, 1, flat)],
        ),
        Scenario::load(
            "topics-8",
            "8 independent topics, flat out",
            vec![Group::new("all", 8, 1, flat)],
        ),
        Scenario::load(
            "topics-8-qos1",
            "8 independent topics, QoS 1 with acknowledgements",
            vec![Group::new("all", 8, 1, confirmed)],
        ),
        Scenario::load(
            "topics-1000",
            "1,000 topics with a subscriber each, fed by 8 publishers that roam over them",
            vec![Group::new("all", 1000, 1, flat).with_publishers(Publishers::Roaming(8))],
        ),
        Scenario::load(
            "wildcard-100",
            "One wildcard subscriber over 100 topics, fed by 4 roaming publishers",
            vec![
                Group::new("all", 100, 0, flat)
                    .with_publishers(Publishers::Roaming(4))
                    .with_patterns(1),
            ],
        ),
        Scenario::load(
            "shared-4",
            "A shared subscription of 4 workers on one topic, QoS 1",
            vec![Group::new("all", 1, 4, confirmed).sharing()],
        ),
        Scenario::load(
            "rate-1k",
            "Latency on one topic at 1,000 messages a second",
            one(Mode::Rate(1_000)),
        ),
        Scenario::load(
            "rate-10k",
            "Latency on one topic at 10,000 messages a second",
            one(Mode::Rate(10_000)),
        ),
        Scenario::load(
            "fanout-100-rate-1k",
            "Latency for 100 subscribers on one topic at 1,000 messages a second",
            vec![Group::new("all", 1, 100, Mode::Rate(1_000))],
        ),
        Scenario::load(
            "quiet-alone",
            "A topic at 200 messages a second with nothing else running",
            vec![quiet()],
        ),
        Scenario::load(
            "quiet-beside-fanout",
            "The same quiet topic while another topic fans out to 1,000 subscribers",
            vec![quiet(), Group::new("busy", 1, 1000, flat)],
        ),
        Scenario::load(
            "quiet-beside-paced-fanout",
            "The same quiet topic while another topic sends 300 messages a second to 1,000 subscribers",
            vec![quiet(), Group::new("busy", 1, 1000, Mode::Rate(300))],
        ),
        Scenario::load(
            "quiet-beside-topics",
            "The same quiet topic while 8 other topics publish flat out",
            vec![quiet(), Group::new("busy", 8, 1, flat)],
        ),
        Scenario::load(
            "slow-consumer",
            "4 subscribers at 2,000 messages a second beside one that reads only 500 a second",
            vec![Group::new("fast", 1, 4, Mode::Rate(2_000)).with_slow(1)],
        ),
        Scenario::load(
            "idle-10000",
            "The quiet topic while 10,000 other connections are subscribed and idle",
            vec![
                quiet(),
                Group::new("idle", 1, 10_000, flat).with_publishers(Publishers::PerTopic(0)),
            ],
        ),
        Scenario {
            name: "replay-200k",
            about: "Store 200,000 messages, restart the server, then read them all back",
            kind: Kind::Replay { messages: 200_000 },
            groups: Vec::new(),
            settings: Vec::new(),
            default_retention: false,
        },
    ]
}

/// Runs one scenario against a fresh server and measures it.
///
/// Args:
///     scenario: What to measure.
///     options: The binary, backend, timings and payload size.
///
/// Returns:
///     The measurements.
///
/// Raises:
///     String: When the server can't be started or any part of the setup fails.
pub async fn run(scenario: &Scenario, options: &RunOptions) -> Result<ScenarioResult, String> {
    match scenario.kind {
        Kind::Load => run_load(scenario, options).await,
        Kind::Replay { messages } => run_replay(scenario, options, messages).await,
    }
}

/// One subscriber connection to open during setup.
struct SubscriberPlan {
    group: usize,
    subscription: String,
    from: StartFrom,
    share: Option<&'static str>,
    slow: bool,
}

/// Applies a steady load for the run's window and measures it.
///
/// Setup creates one subscribing account and one account per publisher, so publishers don't
/// share a rate limiter, then opens and subscribes every connection before any traffic starts.
/// After the window, publishers stop and the run waits for deliveries still on their way.
///
/// Args:
///     scenario: The load to apply.
///     options: The binary, backend, timings and payload size.
///
/// Returns:
///     The measurements.
///
/// Raises:
///     String: When the server can't be started or any part of the setup fails.
async fn run_load(scenario: &Scenario, options: &RunOptions) -> Result<ScenarioResult, String> {
    let server = match options.target {
        Target::Popsub => {
            ServerProcess::start(
                &options.binary,
                options.backend,
                &scenario.server_settings(),
            )
            .await?
        }
        Target::Nats => ServerProcess::start_nats(&options.binary, false).await?,
        Target::NatsJetstream => ServerProcess::start_nats(&options.binary, true).await?,
        Target::Mosquitto => ServerProcess::start_mosquitto(&options.binary).await?,
    };

    let totals: Vec<Arc<Totals>> = scenario
        .groups
        .iter()
        .map(|_| Arc::new(Totals::default()))
        .collect();
    let mut plans = Vec::new();
    let mut publisher_topics: Vec<(usize, Vec<String>)> = Vec::new();
    for (index, group) in scenario.groups.iter().enumerate() {
        let prefix = format!("bench/{}/{}", scenario.name, group.label);
        let topics: Vec<String> = (0..group.topics).map(|n| format!("{prefix}/{n}")).collect();
        for topic in &topics {
            for n in 0..group.subscribers + group.slow_subscribers {
                let slow = n >= group.subscribers;
                plans.push(SubscriberPlan {
                    group: index,
                    subscription: topic.clone(),
                    from: StartFrom::New,
                    share: (group.shared && !slow).then_some("workers"),
                    slow,
                });
            }
        }
        for _ in 0..group.pattern_subscribers {
            plans.push(SubscriberPlan {
                group: index,
                subscription: format!("{prefix}/#"),
                from: StartFrom::Seq(1),
                share: None,
                slow: false,
            });
        }
        match group.publishers {
            Publishers::PerTopic(count) => {
                for topic in &topics {
                    for _ in 0..count {
                        publisher_topics.push((index, vec![topic.clone()]));
                    }
                }
            }
            Publishers::Roaming(count) => {
                for _ in 0..count {
                    publisher_topics.push((index, topics.clone()));
                }
            }
        }
    }

    let mut notes = Vec::new();
    let connecting = Instant::now();
    let (subscriber_links, publisher_links) = match options.target {
        Target::Popsub => connect_popsub(&server.url(), &plans, publisher_topics.len()).await?,
        Target::Nats | Target::NatsJetstream => {
            let jetstream = options.target == Target::NatsJetstream;
            connect_nats(&server.address(), jetstream, &plans, publisher_topics.len()).await?
        }
        Target::Mosquitto => {
            connect_mqtt(&server.address(), scenario, &plans, publisher_topics.len()).await?
        }
    };
    if plans.len() >= MANY_CONNECTIONS {
        let seconds = connecting.elapsed().as_secs_f64();
        notes.push(format!(
            "{}: opened and subscribed {} connections in {seconds:.2} s ({} a second, {CONNECT_AT_ONCE} at a time).",
            scenario.name,
            thousands(plans.len() as f64),
            thousands(plans.len() as f64 / seconds),
        ));
    }

    let clock = Clock::start();
    let begin_ns = clock.now_ns() + nanos(options.warmup);
    let window = Window {
        begin_ns,
        end_ns: begin_ns + nanos(options.duration),
    };
    let (stop, stopped) = watch::channel(false);

    let mut subscribers = Vec::new();
    for (plan, link) in plans.iter().zip(subscriber_links) {
        let group = &scenario.groups[plan.group];
        let spec = SubscriberSpec {
            acknowledge: group.mode.in_flight() > 0,
            record_latency: group.mode != Mode::FlatOut && !plan.slow,
            delay: plan.slow.then_some(SLOW_DELAY),
        };
        let counted = (!plan.slow).then(|| Arc::clone(&totals[plan.group]));
        let stopped = stopped.clone();
        let task = match link {
            Link::Popsub(socket) => tokio::spawn(popsub::subscribe(
                *socket, spec, clock, window, counted, stopped,
            )),
            Link::Nats(conn) => {
                tokio::spawn(nats::subscribe(conn, spec, clock, window, counted, stopped))
            }
            Link::Mqtt(conn) => {
                tokio::spawn(mqtt::subscribe(conn, spec, clock, window, counted, stopped))
            }
        };
        if !plan.slow {
            subscribers.push((plan.group, task));
        }
    }
    let mut publishers = Vec::new();
    for ((index, topics), link) in publisher_topics.into_iter().zip(publisher_links) {
        let group = &scenario.groups[index];
        let spec = PublisherSpec {
            topics,
            mode: group.mode,
            payload_bytes: group.payload_bytes.unwrap_or(options.payload_bytes),
        };
        let totals = Arc::clone(&totals[index]);
        let task: tokio::task::JoinHandle<Publisher> = match link {
            Link::Popsub(socket) => {
                tokio::spawn(popsub::publish(*socket, spec, clock, window, totals))
            }
            Link::Nats(conn) => tokio::spawn(nats::publish(conn, spec, clock, window, totals)),
            Link::Mqtt(conn) => tokio::spawn(mqtt::publish(conn, spec, clock, window, totals)),
        };
        publishers.push((index, task));
    }

    let own_pid = std::process::id();
    let mut over = sleep_watching(server.pid(), clock.instant(window.begin_ns)).await;
    let server_before = cpu_seconds(server.pid());
    let own_before = cpu_seconds(own_pid);
    if over.is_none() {
        over = sleep_watching(server.pid(), clock.instant(window.end_ns)).await;
    }
    if let Some(mib) = over {
        notes.push(format!(
            "{}: stopped early. The broker's memory reached {} MiB, past the benchmark's limit of {} MiB.",
            scenario.name,
            thousands(mib),
            thousands(MEMORY_LIMIT_MIB),
        ));
        return Ok(ScenarioResult {
            name: scenario.name,
            groups: Vec::new(),
            server_cores: None,
            loadgen_cores: None,
            server_rss_mib: Some(mib),
            notes,
        });
    }
    let server_after = cpu_seconds(server.pid());
    let own_after = cpu_seconds(own_pid);
    let server_rss_mib = resident_mib(server.pid());

    let mut finished = Vec::new();
    for (group, task) in publishers {
        finished.push((group, task.await.map_err(|e| e.to_string())?));
    }
    drain(&totals, &scenario.groups).await;
    let _ = stop.send(true);
    let mut seen: Vec<(usize, SubscriberStats)> = Vec::new();
    for (group, task) in subscribers {
        seen.push((group, task.await.map_err(|e| e.to_string())?));
    }

    let seconds = window.seconds();
    let mut groups = Vec::new();
    for (index, group) in scenario.groups.iter().enumerate() {
        let published: u64 = finished
            .iter()
            .filter(|(owner, _)| *owner == index)
            .map(|(_, publisher)| publisher.published_in_window())
            .sum();
        let mut errors: u64 = finished
            .iter()
            .filter(|(owner, _)| *owner == index)
            .map(|(_, publisher)| publisher.errors())
            .sum();
        let mut delivered = 0;
        let mut merged: Option<Histogram<u64>> = None;
        for (_, stats) in seen.iter().filter(|(owner, _)| *owner == index) {
            delivered += stats.in_window;
            errors += stats.errors;
            if let Some(latency) = &stats.latency {
                match &mut merged {
                    Some(total) => {
                        let _ = total.add(latency);
                    }
                    None => merged = Some(latency.clone()),
                }
            }
        }
        groups.push(GroupResult {
            label: group.label,
            load: group.describe(),
            mode: match group.publishers {
                Publishers::PerTopic(0) => "idle".to_string(),
                _ => group.mode.label(),
            },
            publishes_per_sec: published as f64 / seconds,
            deliveries_per_sec: delivered as f64 / seconds,
            latency: merged.filter(|h| !h.is_empty()).map(|h| Latency {
                p50_ms: h.value_at_quantile(0.5) as f64 / 1_000.0,
                p99_ms: h.value_at_quantile(0.99) as f64 / 1_000.0,
                max_ms: h.max() as f64 / 1_000.0,
            }),
            errors,
            complete: totals[index].received.load(Ordering::Relaxed) >= owed(&totals[index], group),
        });
    }
    drop(finished);

    Ok(ScenarioResult {
        name: scenario.name,
        groups,
        server_cores: cores(server_before, server_after, seconds),
        loadgen_cores: cores(own_before, own_after, seconds),
        server_rss_mib,
        notes,
    })
}

/// Stores messages with nobody subscribed, restarts the server, and times reading them back.
///
/// The publisher sends QoS 0 flat out, then one confirmed publish. A connection's frames are
/// handled in order, so that reply means everything before it is stored, and its `seq` is the
/// number of messages in the log. With the file backend the server is then stopped and started
/// on the same data directory. Finally one subscriber asks for the topic from `seq` 1 and the
/// time until the last message arrives is measured.
///
/// Args:
///     scenario: The replay scenario, for its name and settings.
///     options: The binary, backend and payload size.
///     messages: How many messages to store before the confirmed one.
///
/// Returns:
///     One row: the storing rate as publishes a second and the replay rate as deliveries a
///     second, with the restart time as a note.
///
/// Raises:
///     String: When any step fails or the replay takes longer than `REPLAY_LIMIT`.
async fn run_replay(
    scenario: &Scenario,
    options: &RunOptions,
    messages: u64,
) -> Result<ScenarioResult, String> {
    let mut server = ServerProcess::start(
        &options.binary,
        options.backend,
        &scenario.server_settings(),
    )
    .await?;
    let url = server.url();
    let topic = format!("bench/{}/log", scenario.name);
    let admin_token = login(&url, "admin", "password").await?;
    let mut admin = open(&url, &admin_token).await?;
    create_account(&mut admin, 10, "bench-sub", Access::Subscribe).await?;
    create_account(&mut admin, 11, "bench-pub-0", Access::Publish).await?;
    drop(admin);

    let publisher_token = login(&url, "bench-pub-0", BENCH_PASSWORD).await?;
    let mut publisher = open(&url, &publisher_token).await?;
    let template = PublishTemplate::new(&topic, 0, options.payload_bytes);
    let storing = Instant::now();
    for n in 0..messages {
        publisher
            .feed(Message::text(template.render(n, None)))
            .await
            .map_err(|e| format!("storing messages failed: {e}"))?;
        if n % 64 == 63 {
            publisher.flush().await.map_err(|e| e.to_string())?;
        }
    }
    let last = request(
        &mut publisher,
        5,
        ClientMessage::Publish {
            topic: topic.clone(),
            payload: "end".to_string(),
            encoding: None,
            message_id: None,
            qos: Some(1),
        },
    )
    .await?;
    let ServerMessage::Published { seq: last_seq, .. } = last else {
        return Err(format!("unexpected reply to the last publish: {last:?}"));
    };
    let store_rate = last_seq as f64 / storing.elapsed().as_secs_f64();
    drop(publisher);

    let mut notes = Vec::new();
    if options.backend == Backend::File {
        let took = server.restart().await?;
        notes.push(format!(
            "{}: after a restart with {} stored messages the server accepted connections in {} ms.",
            scenario.name,
            thousands(last_seq as f64),
            took.as_millis(),
        ));
    }

    let subscriber_token = login(&url, "bench-sub", BENCH_PASSWORD).await?;
    let mut subscriber = open(&url, &subscriber_token).await?;
    let own_pid = std::process::id();
    let server_before = cpu_seconds(server.pid());
    let own_before = cpu_seconds(own_pid);
    let replaying = Instant::now();
    let frame = ClientFrame::with_id(
        RequestId::Number(2),
        ClientMessage::Subscribe {
            topic: topic.clone(),
            from: StartFrom::Seq(1),
            group: None,
        },
    );
    subscriber
        .send(Message::text(
            serde_json::to_string(&frame).map_err(|e| e.to_string())?,
        ))
        .await
        .map_err(|e| format!("subscribing failed: {e}"))?;
    let read_all = async {
        let mut received: u64 = 0;
        while let Some(Ok(frame)) = subscriber.next().await {
            let Message::Text(text) = frame else {
                continue;
            };
            if let Some(seq) = delivery_seq(text.as_str()) {
                received += 1;
                if seq >= last_seq {
                    break;
                }
            }
        }
        received
    };
    let received = timeout(REPLAY_LIMIT, read_all)
        .await
        .map_err(|_| "the replay didn't finish in time".to_string())?;
    let seconds = replaying.elapsed().as_secs_f64();
    let server_after = cpu_seconds(server.pid());
    let own_after = cpu_seconds(own_pid);

    Ok(ScenarioResult {
        name: scenario.name,
        groups: vec![GroupResult {
            label: "all",
            load: format!(
                "1 topic, {} stored messages, 1 subscriber from seq 1",
                thousands(last_seq as f64)
            ),
            mode: "store, then replay".to_string(),
            publishes_per_sec: store_rate,
            deliveries_per_sec: received as f64 / seconds,
            latency: None,
            errors: 0,
            complete: received == last_seq,
        }],
        server_cores: cores(server_before, server_after, seconds),
        loadgen_cores: cores(own_before, own_after, seconds),
        server_rss_mib: resident_mib(server.pid()),
        notes,
    })
}

/// Opens every PopSub connection a scenario needs.
///
/// Creates one subscribing account and one account per publisher, so publishers don't share a
/// rate limiter. Subscribers are opened `CONNECT_AT_ONCE` at a time and are subscribed before
/// this returns.
///
/// Args:
///     url: The server's WebSocket URL.
///     plans: One entry per subscriber connection.
///     publishers: How many publisher connections to open.
///
/// Returns:
///     The subscriber links in the order of `plans`, then the publisher links.
///
/// Raises:
///     String: When an account can't be created or a connection fails to open or subscribe.
async fn connect_popsub(
    url: &str,
    plans: &[SubscriberPlan],
    publishers: usize,
) -> Result<(Vec<Link>, Vec<Link>), String> {
    let admin_token = login(url, "admin", "password").await?;
    let mut admin = open(url, &admin_token).await?;
    create_account(&mut admin, 10, "bench-sub", Access::Subscribe).await?;
    let token = login(url, "bench-sub", BENCH_PASSWORD).await?;
    let token = token.as_str();
    let opened: Vec<Result<Link, String>> = stream::iter(plans)
        .map(|plan| async move {
            let mut socket = open(url, token).await?;
            subscribe(&mut socket, &plan.subscription, plan.from, plan.share).await?;
            Ok(Link::Popsub(Box::new(socket)))
        })
        .buffered(CONNECT_AT_ONCE)
        .collect()
        .await;
    let subscribers = opened.into_iter().collect::<Result<Vec<_>, _>>()?;

    let mut links = Vec::new();
    for account in 0..publishers as u64 {
        let username = format!("bench-pub-{account}");
        create_account(&mut admin, 100 + account, &username, Access::Publish).await?;
        let token = login(url, &username, BENCH_PASSWORD).await?;
        links.push(Link::Popsub(Box::new(open(url, &token).await?)));
    }
    Ok((subscribers, links))
}

/// Opens every NATS connection a scenario needs.
///
/// Args:
///     address: The server's `host:port`.
///     jetstream: Whether to create the stream that stores every benchmark subject first.
///     plans: One entry per subscriber connection.
///     publishers: How many publisher connections to open.
///
/// Returns:
///     The subscriber links in the order of `plans`, then the publisher links.
///
/// Raises:
///     String: When the stream can't be created or a connection fails to open or subscribe.
async fn connect_nats(
    address: &str,
    jetstream: bool,
    plans: &[SubscriberPlan],
    publishers: usize,
) -> Result<(Vec<Link>, Vec<Link>), String> {
    if jetstream {
        nats::create_stream(address).await?;
    }
    let opened: Vec<Result<Link, String>> = stream::iter(plans)
        .map(|plan| async move {
            let mut conn = nats::connect(address).await?;
            nats::subscribe_to(&mut conn, &plan.subscription).await?;
            Ok(Link::Nats(conn))
        })
        .buffered(CONNECT_AT_ONCE)
        .collect()
        .await;
    let subscribers = opened.into_iter().collect::<Result<Vec<_>, _>>()?;
    let mut links = Vec::new();
    for _ in 0..publishers {
        links.push(Link::Nats(nats::connect(address).await?));
    }
    Ok((subscribers, links))
}

/// Opens every MQTT connection a scenario needs.
///
/// Subscribers of a group whose publishes are confirmed subscribe at QoS 1, the rest at QoS 0.
///
/// Args:
///     address: The broker's `host:port`.
///     scenario: The scenario, for each group's pacing.
///     plans: One entry per subscriber connection.
///     publishers: How many publisher connections to open.
///
/// Returns:
///     The subscriber links in the order of `plans`, then the publisher links.
///
/// Raises:
///     String: When a connection fails to open or subscribe.
async fn connect_mqtt(
    address: &str,
    scenario: &Scenario,
    plans: &[SubscriberPlan],
    publishers: usize,
) -> Result<(Vec<Link>, Vec<Link>), String> {
    let opened: Vec<Result<Link, String>> = stream::iter(plans)
        .map(|plan| async move {
            let qos = u8::from(scenario.groups[plan.group].mode.in_flight() > 0);
            let mut conn = mqtt::connect(address).await?;
            mqtt::subscribe_to(&mut conn, &plan.subscription, qos).await?;
            Ok(Link::Mqtt(conn))
        })
        .buffered(CONNECT_AT_ONCE)
        .collect()
        .await;
    let subscribers = opened.into_iter().collect::<Result<Vec<_>, _>>()?;
    let mut links = Vec::new();
    for _ in 0..publishers {
        links.push(Link::Mqtt(mqtt::connect(address).await?));
    }
    Ok((subscribers, links))
}

/// How many deliveries a group's counted subscribers are owed in total.
///
/// Args:
///     totals: The group's totals, with its count of publishes sent.
///     group: The group, for how many deliveries each publish causes.
///
/// Returns:
///     Publishes sent multiplied by deliveries per publish.
fn owed(totals: &Totals, group: &Group) -> u64 {
    totals.sent.load(Ordering::Relaxed) * group.deliveries_per_publish()
}

/// Waits, after publishers have stopped, for the deliveries still on their way.
///
/// Returns when every group has received what it is owed, when nothing new has arrived for
/// `DRAIN_QUIET`, or after `DRAIN_LIMIT`.
///
/// Args:
///     totals: Every group's totals.
///     groups: The scenario's groups, in the same order.
async fn drain(totals: &[Arc<Totals>], groups: &[Group]) {
    let received = || -> u64 {
        totals
            .iter()
            .map(|group| group.received.load(Ordering::Relaxed))
            .sum()
    };
    let expected: u64 = totals
        .iter()
        .zip(groups)
        .map(|(totals, group)| owed(totals, group))
        .sum();
    let mut last = received();
    let mut quiet = Duration::ZERO;
    let mut waited = Duration::ZERO;
    while last < expected && quiet < DRAIN_QUIET && waited < DRAIN_LIMIT {
        sleep(DRAIN_POLL).await;
        waited += DRAIN_POLL;
        let now = received();
        if now == last {
            quiet += DRAIN_POLL;
        } else {
            quiet = Duration::ZERO;
            last = now;
        }
    }
}

/// Sleeps until an instant, checking the broker's memory every `MEMORY_POLL`.
///
/// Args:
///     pid: The broker's process id.
///     until: When to stop waiting.
///
/// Returns:
///     `None` when the instant was reached, or the broker's memory in mebibytes as soon as it
///     passes `MEMORY_LIMIT_MIB`.
async fn sleep_watching(pid: u32, until: Instant) -> Option<f64> {
    loop {
        let now = Instant::now();
        if now >= until {
            return None;
        }
        sleep_until(until.min(now + MEMORY_POLL)).await;
        if let Some(mib) = resident_mib(pid)
            && mib > MEMORY_LIMIT_MIB
        {
            return Some(mib);
        }
    }
}

/// Turns two CPU time readings into an average number of cores in use.
///
/// Args:
///     before: CPU seconds at the start of measuring.
///     after: CPU seconds at its end.
///     seconds: How long measuring took.
///
/// Returns:
///     CPU seconds used divided by wall seconds, or `None` when either reading is missing.
fn cores(before: Option<f64>, after: Option<f64>, seconds: f64) -> Option<f64> {
    Some((after? - before?) / seconds)
}

/// Reads a process's resident memory in mebibytes.
///
/// Args:
///     pid: The process to read.
///
/// Returns:
///     The resident set size, or `None` when it can't be read.
fn resident_mib(pid: u32) -> Option<f64> {
    resident_bytes(pid).map(|bytes| bytes as f64 / 1_048_576.0)
}

/// Converts a duration to whole nanoseconds.
///
/// Args:
///     duration: The duration.
///
/// Returns:
///     Its length in nanoseconds, saturating at `u64::MAX`.
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
