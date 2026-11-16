# PopSub storage design

| | |
|---|---|
| Status | Being implemented. Steps 1 (the `popsub_storage` crate), 2 (the broker on the log) and 3 (sessions and cursors) are done |
| Owner | Soumyadeep Ghosh |
| Replaces | `popsub_persistence` (the `sled` replay store) and the in-memory `pending_acks` table |
| Related | `docs/DESIGN.md` sections 6, 7, 12 and 13 (phase 1) |

## 1. Summary

PopSub needs two things from storage: a **replay buffer**, so a new subscriber can catch up on recent messages, and **durability**, so a QoS 1 message reaches every subscriber at least once even across disconnects and restarts. Operators should also be able to choose where messages live, including storage PopSub doesn't ship.

This proposal meets all three with one idea: **every topic is an append-only log of numbered records, and every durable subscriber has a cursor into it.**

- Replay is reading the recent end of the log.
- Durability is the subscriber's cursor plus a rule for how firmly an append must be stored before the publish is confirmed.
- Pluggable storage is a small trait for the log and the cursors, implemented by the built-in backends and by anyone else, and held to the same conformance test suite.

## 2. Goals and non-goals

**Goals**

1. Replay recent history to new subscribers, bounded by count, age or size.
2. At-least-once delivery for QoS 1 per subscriber, surviving client disconnects and server restarts, when the backend is durable.
3. Resume from where a subscriber left off, without replaying what it has already acknowledged.
4. Let operators pick a built-in backend in config, or plug in their own by implementing a trait.
5. One conformance suite that every backend, built in or third party, must pass.
6. Keep the default (no extra infrastructure) a single binary and a directory on disk.

**Non-goals**

- Exactly-once delivery. Consumers deduplicate by `(topic, seq)`.
- Replication or clustering. A backend may be replicated underneath (Postgres can be), but PopSub itself stays single node. The log design is chosen so clustering can build on it later.
- Transactions across topics.
- Binary payloads. Payloads stay strings; the record format leaves room to change that.

## 3. Concepts

| Term | Meaning |
|---|---|
| **Topic log** | An append-only, totally ordered sequence of records for one topic. |
| **Sequence number (`seq`)** | A record's position in its topic log. Starts at 1, increases by exactly 1, never reused, even after the record is trimmed. |
| **Retention** | Which records a topic log keeps: at most N records, records younger than T, or at most B bytes. Older records are trimmed from the front. |
| **Session** | A client identity that outlives a connection, chosen by the client (`session_id`). Connections without one are ephemeral, as today. |
| **Subscription** | A session's interest in a topic, plus its **cursor**: the highest `seq` it has acknowledged without gaps. |
| **In-flight window** | QoS 1 records sent to a subscription but not yet acknowledged, capped at `max_inflight`. Kept in memory and rebuilt from the cursor after a restart. |
| **Durability level** | How firmly an append must be stored before the publisher is told it succeeded: `none`, `buffered` or `synced`. |

## 4. What changes, and why

| Today | Problem | With this design |
|---|---|---|
| `sled` tree per topic, keyed by timestamp plus a random id | No sequence numbers, so no "resume from here" | Every record has a `seq`; subscribers resume from their cursor |
| Stored record is `{topic, payload, timestamp}` | Message id and QoS are lost, so replays can't be matched or acknowledged | The full record is stored |
| Every subscribe replays the whole history with fresh ids | Duplicates on every reconnect, and clients can't tell | Subscribe says where to start; replays carry their original `seq` |
| `pending_acks` keyed by message id, in memory | One ACK clears a message for all subscribers; a restart loses everything | Delivery state is per subscription and derived from durable cursors |
| `sled` flushes in the background every 500 ms | A crash can lose messages a publisher thinks were stored | The operator picks a durability level; QoS 1 publishes are confirmed only after it is met |
| Expiry happens only when a topic is read | Unread topics grow until the cap | Retention is enforced on append and by a background task |
| Storage is hard-wired to `sled` | Nobody can bring their own store | A trait, built-in backends, and a conformance suite |

## 5. Data model

```rust
/// A record as stored in a topic log.
pub struct Record {
    pub topic: String,
    pub seq: u64,
    pub message_id: String,
    pub publisher: String,
    pub payload: String,
    pub qos: u8,
    pub published_at_ms: i64,
}

/// A durable subscription and how far it has acknowledged.
pub struct Subscription {
    pub session_id: String,
    pub topic: String,
    pub acked_seq: u64,
}
```

- `seq` is assigned by the log at append time, never by the publisher.
- `message_id` is the publisher's idempotency key (or a generated UUID). Within one `(topic, publisher)`, a repeated `message_id` returns the original `seq` instead of appending again, so a publisher retrying after a lost confirmation doesn't create duplicates. Backends keep this lookup for a bounded window (default: the retention window).
- A subscription's `acked_seq` only moves forward.

## 6. The storage interface

A new crate, `popsub_storage`, owns the types above, the traits below, the built-in backends and the conformance suite. Storage calls are `async`, because real backends do I/O (a network round trip for Postgres, `fsync` for files).

```rust
#[async_trait]
pub trait MessageLog: Send + Sync {
    /// Appends a record and returns it as stored (with its seq and final
    /// publish time), or the original record if (topic, publisher,
    /// message_id) is still retained.
    async fn append(&self, topic: &str, record: NewRecord, durability: Durability) -> Result<Record>;

    /// Returns up to `max` records with seq >= from, in seq order.
    async fn read(&self, topic: &str, from: u64, max: usize) -> Result<Vec<Record>>;

    /// The first retained seq and the last seq ever appended. An unknown
    /// topic is 1..=0; a fully trimmed one keeps its last seq.
    async fn bounds(&self, topic: &str) -> Result<LogBounds>;

    /// The first seq published at or after `time_ms`, for "replay since".
    async fn seq_at(&self, topic: &str, time_ms: i64) -> Result<Option<u64>>;

    /// Trims records outside the policy and returns how many were removed.
    /// `now_ms` is passed in so the age limit is testable.
    async fn trim(&self, topic: &str, policy: &Retention, now_ms: i64) -> Result<u64>;

    /// Every topic that has a log.
    async fn topics(&self) -> Result<Vec<String>>;
}

#[async_trait]
pub trait SubscriptionStore: Send + Sync {
    /// Every subscription a session holds, with cursors.
    async fn load(&self, session_id: &str) -> Result<Vec<Subscription>>;

    /// Creates or replaces a subscription starting after `acked_seq`.
    async fn subscribe(&self, session_id: &str, topic: &str, acked_seq: u64) -> Result<()>;

    async fn unsubscribe(&self, session_id: &str, topic: &str) -> Result<()>;

    /// Moves the cursor forward. A lower or equal value is ignored.
    async fn ack(&self, session_id: &str, topic: &str, seq: u64) -> Result<()>;

    /// Removes sessions not seen since `before_ms`, returning how many.
    async fn expire(&self, before_ms: i64) -> Result<u64>;

    /// Records that a session was active, for expiry.
    async fn touch(&self, session_id: &str, at_ms: i64) -> Result<()>;
}

/// What a backend provides: both halves, plus what it can promise.
pub trait Storage: MessageLog + SubscriptionStore {
    fn capabilities(&self) -> Capabilities;
}

pub struct Capabilities {
    /// The strongest durability level the backend can honour.
    pub max_durability: Durability,
    /// Whether data survives a process restart.
    pub persistent: bool,
}

pub enum Durability { None, Buffered, Synced }
```

`LogBounds` keeps the last `seq` even when every record has been trimmed, so a caller can always tell what the next `seq` will be (the broker needs that for `from: "new"`). Each log also keeps publish times non-decreasing: a record whose `published_at_ms` is earlier than the previous record's is stored with the previous record's time, so `seq_at` is a binary search.

**Why two traits.** Logs and cursors have different access patterns (sequential append and scan versus small random updates). Splitting them lets a backend implement each well, and lets a future setup keep logs in one place and cursors in another. Most backends implement both on one type.

**Errors.** One `StorageError` with variants `Unavailable` (retry may help), `Corrupt` (data failed a checksum), `Unsupported` (for example `Synced` on a backend that can't), and `Other`. The server never panics on a storage error; see section 10.

**Plugging in your own.** The server is built with any `Arc<dyn Storage>`:

```rust
let storage: Arc<dyn Storage> = Arc::new(MyRedisStorage::connect(url).await?);
Server::from_settings(broker, &settings.server).await.with_storage(storage)
```

Built-in backends are also selectable from config (section 9), so most operators never write code.

## 7. Built-in backends

| Backend | Feature flag | Persistent | Max durability | For |
|---|---|---|---|---|
| `memory` | always | No | `none` | Tests, demos, pure replay buffer with no disk |
| `file` | default | Yes | `synced` | The default: one binary, one directory |
| `postgres` | `postgres` | Yes | `synced` | Operators who already run Postgres and want backups and replicas |

### 7.1 `memory`

A `VecDeque<Record>` per topic and a `HashMap` of subscriptions. Restarting loses everything. Configuring a stronger durability than `none` with it is a startup error, not a silent downgrade.

### 7.2 `file`

Replaces `sled`. One directory per topic, holding **segment files** of records:

- **Record framing:** `length (u32) | crc32 (u32) | encoded record`. The CRC covers the encoded bytes.
- **Segments** roll at a size limit (default 64 MB). Each segment is named by its first `seq`, so finding a record is a directory listing plus a binary search, and trimming deletes whole segments from the front.
- **A sparse index** per segment maps every Nth `seq` to a byte offset, rebuilt from the segment if missing.
- **Recovery:** on open, the last segment is scanned from its last indexed offset; the first record with a bad length or CRC marks a torn write, and the file is truncated there. Nothing confirmed as `synced` can be lost this way, because it was `fsync`ed before confirmation.
- **Durability:** `buffered` writes go to the OS page cache. `synced` uses **group commit**: appends wait on a shared `fsync` that runs every few milliseconds or when a batch fills, so throughput doesn't drop to one `fsync` per message.
- **Subscriptions** live in a small append-only file of cursor updates, compacted when it grows past a threshold.
- **Concurrency:** one writer task per topic owns that topic's active segment, which also gives the per-topic ordering in section 8.

### 7.3 `postgres`

```sql
CREATE TABLE popsub_records (
    topic TEXT NOT NULL,
    seq BIGINT NOT NULL,
    message_id TEXT NOT NULL,
    publisher TEXT NOT NULL,
    payload TEXT NOT NULL,
    qos SMALLINT NOT NULL,
    published_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (topic, seq),
    UNIQUE (topic, publisher, message_id)
);
CREATE TABLE popsub_topic_heads (topic TEXT PRIMARY KEY, last_seq BIGINT NOT NULL);
CREATE TABLE popsub_subscriptions (
    session_id TEXT NOT NULL,
    topic TEXT NOT NULL,
    acked_seq BIGINT NOT NULL,
    PRIMARY KEY (session_id, topic)
);
CREATE TABLE popsub_sessions (session_id TEXT PRIMARY KEY, last_seen TIMESTAMPTZ NOT NULL);
```

- `seq` comes from incrementing `popsub_topic_heads.last_seq` in the same transaction as the insert, so sequences are gap-free per topic. (A Postgres `SEQUENCE` would leave gaps on rollback.)
- `synced` means the transaction committed. `buffered` uses `synchronous_commit = off` for that transaction.
- The publisher-retry guarantee comes from the `UNIQUE` constraint: on conflict, the existing `seq` is returned.
- Migrations run one statement at a time (the lesson from the auth store).

### 7.4 Third-party backends

Anything that implements `Storage` and passes the conformance suite: Redis Streams, S3 with a local cache, SQLite, a cloud database. They live outside this repo, and their own tests call the suite (section 11).

## 8. How delivery works

### 8.1 Publish

1. The session checks permission and rate limit (unchanged).
2. It sends the record to the topic's writer, which appends it at the configured durability and gets a `seq`. No broker lock is held during the append.
3. For QoS 1, the publisher gets `{"type":"published","message_id":"...","seq":42}` once the append meets the durability level. A publisher that doesn't see it retries with the same `message_id`, and the log returns the original `seq` instead of a duplicate.
4. The writer fans the record out to connected subscribers of the topic, in `seq` order.

Ordering: per topic, every subscriber sees records in `seq` order, live and on replay. There is no ordering across topics.

### 8.2 Subscribe and replay

`subscribe` gains an optional `from`:

| `from` | Starts at |
|---|---|
| `"new"` (default for ephemeral connections) | Only records published after subscribing. No replay. |
| `{"last": 50}` | The last 50 retained records |
| `{"since_ms": 1794800000000}` | The first record at or after that time |
| `{"seq": 1200}` | That `seq`, or the first retained one if it was trimmed |
| `"resume"` (default for sessions with a stored subscription) | One past the subscription's cursor |

Replay is read from the log in pages and sent before live records, without gaps or overlaps between the two. As built, a subscription remembers the next `seq` it expects and is only ever sent that one; anything it misses (because it is replaying, or its window is full) is read back from the log by the topic's writer rather than buffered in memory. The client is first sent `{"type":"subscribed","topic":"chat","replay_from":41,"replay_to":90}`, so it knows which records are history.

If the requested start has already been trimmed, the subscriber gets `{"type":"truncated","topic":"chat","first_available":1300}` before the records, so it knows it missed something. **Retention always wins over slow subscribers**: storage stays bounded, and data loss is visible rather than silent.

### 8.3 Acknowledgement and retry

- Deliveries become `{"type":"message","topic":"chat","seq":42,"message_id":"...","payload":"...","qos":1,"published_at_ms":...}`. This also fixes today's untagged delivery frames.
- A client acknowledges with `{"type":"ack","topic":"chat","seq":42}`. Acks are only accepted for records sent to that session, which closes today's "anyone can ack anything" gap.
- The cursor advances to the highest contiguous acknowledged `seq`. Out-of-order acks above it are held in memory until the gap fills.
- At most `max_inflight` unacknowledged records are outstanding per subscription. A record not acknowledged within `ack_timeout_ms` is resent, up to `max_retries`; after that, the subscription is reported to the client as `{"type":"stalled","topic":"chat","seq":42}` and delivery for it pauses until the client acks or resubscribes. (Today the message is silently dropped.)
- QoS 0 records take no space in the window and need no ACK. They count as done once sent, so they move a durable session's cursor as soon as they go out. While the window is full, QoS 0 records wait too, to keep order.

### 8.4 Sessions

- `auth` gains optional `session_id` and `clean_start` fields. Without `session_id`, the connection is ephemeral and behaves as today, except that deliveries are tagged.
- With a `session_id`, subscriptions and cursors are stored and restored on reconnect, and replay defaults to `resume`. `clean_start: true` discards the stored subscriptions.
- A session is bound to the authenticated username. Another user can't claim it.
- Sessions not seen for `session_expiry_secs` are removed by a background task.

### 8.5 After a restart

The server loads nothing up front. When a session reconnects, its subscriptions are loaded and delivery resumes from each cursor, so everything sent but not acknowledged before the restart is sent again. That is the at-least-once guarantee across restarts.

## 9. Configuration

```toml
[storage]
backend = "file"            # memory | file | postgres
path = "pubsub_data"        # file backend
durability = "buffered"     # none | buffered | synced
session_expiry_secs = 86400

[storage.retention]          # defaults for every topic; any limit may be omitted
max_messages = 1000
max_age_secs = 3600
max_bytes = 67108864

[storage.postgres]
url_env = "POPSUB_DATABASE_URL"   # read the URL from this variable, never from the file

[broker]
ack_timeout_ms = 5000
max_retries = 5
max_inflight = 100
```

- `broker.message_ttl_secs` is replaced by `storage.retention.max_age_secs`, and the hard-coded `pubsub_db` path and 1000-message cap become settings.
- Startup fails, rather than silently downgrading, if `durability` exceeds what the backend can provide.
- Per-topic retention overrides are an open question (section 14).

## 10. Failure handling

| Failure | Behaviour |
|---|---|
| Append fails | QoS 1 publisher gets `{"type":"error","message":"storage unavailable"}` and no `published` frame, so it retries. QoS 0 is logged and dropped. |
| Read fails during replay | Subscriber gets an error for that topic and is not subscribed, so it never receives live records with a gap before them. It can subscribe again. |
| Cursor update fails | Logged and retried in the background. The worst case is resending records the client already acknowledged, which at-least-once allows. |
| `file` finds a corrupt record mid-segment | Startup fails with the segment and offset, rather than serving data that failed its checksum. A repair command can truncate at that point. |
| Disk full | Appends fail as above. The server stays up and keeps delivering what is already stored. |
| Postgres unreachable | Appends and cursor updates fail as above, and a health metric reports it. |

## 11. Conformance suite

`popsub_storage` exports a test suite that any backend runs against itself:

```rust
struct TempDirFixture(tempfile::TempDir);

impl popsub_storage::conformance::Fixture for TempDirFixture {
    type Storage = MyStorage;

    async fn open(&self) -> MyStorage {
        MyStorage::open(self.0.path()).await.unwrap()
    }
}

popsub_storage::conformance_tests!(TempDirFixture(tempfile::tempdir().unwrap()));
```

The suite is behind the `conformance` feature. Each generated test builds a new fixture, and opening the same fixture again must reach the same data, which is how reopen is tested. Checks that need persistence or `synced` pass trivially on backends whose `capabilities()` don't offer them.

It checks, with the backend reopened between steps where persistence matters:

- `seq` starts at 1, increases by one, and is never reused after trimming.
- `read` returns records in order, respects `from` and `max`, and returns nothing past the end.
- Retention by count, age and bytes trims from the front only.
- Appending the same `(topic, publisher, message_id)` twice returns the same `seq`.
- Cursors only move forward, and survive reopen when `persistent` is true.
- Publish times never decrease along a log, and `seq_at` finds the first record at or after a time.
- Concurrent appends to one topic produce one gap-free sequence.
- Appends to different topics don't interfere.
- Durability stronger than the backend offers is refused with `Unsupported` and stores nothing.
- Acks for unknown subscriptions are ignored, and `expire` removes idle sessions with their subscriptions.

The `file` backend also gets a crash test: a child process appends with `synced` durability and is killed at random points, and every confirmed `seq` must be readable afterwards.

## 12. Performance expectations

To be measured, not assumed; the benchmarks will be published with the implementation.

- `memory`: bounded by fan-out, not storage.
- `file`, `buffered`: close to sequential write speed of the disk.
- `file`, `synced`: limited by `fsync` latency, recovered by group commit; batch size and wait time are tunable.
- `postgres`: a round trip per append; batching appends per topic is a later optimisation.

Moving appends out of the broker lock (into per-topic writers) is itself expected to remove today's main contention point.

## 13. Rollout

Stacked pull requests, each merged on its own with tests:

1. **`popsub_storage` crate:** types, traits, `memory` backend, conformance suite. Nothing uses it yet.
2. **Broker on the log:** per-topic writers, `seq` on every record, tagged `message` delivery frames, `subscribe` with `from`, `truncated` frames, the `[storage]` settings and the retention loop. Ephemeral connections only. Protocol change, documented in the README and design doc. Until step 4, the only backend is `memory`, so history no longer survives a restart.
3. **Sessions and cursors:** `session_id` and `clean_start`, per-subscription in-flight windows, `ack` by `topic` and `seq`, `published` confirmations, `stalled` frames, and a `subscribed` reply carrying the replay range. Removes `pending_acks`. Tests for several subscribers, reconnects and acks from the wrong session.
4. **`file` backend:** segments, index, recovery, group commit, crash test. Becomes the default.
5. **`postgres` backend**, run against a real database in CI (a Postgres service container).
6. **Remove `popsub_persistence` and `sled`.** Existing `pubsub_db` data is replay history only, so it is not migrated; the release notes say so.

The delivery semantics table in `docs/DESIGN.md` section 6 is updated in PR 3, where the guarantees actually change.

## 14. Decisions taken for implementation

These were open questions in the proposal. Implementation goes ahead with the proposal's defaults; any of them can still change before the step that depends on it.

1. **Per-topic retention:** one global policy for now. Overrides by topic pattern can be added later without changing the traits, because `trim` already takes the policy per call.
2. **Default for ephemeral subscribers:** `"new"`. This is a visible change from today's replay-everything behaviour and is called out in the README when it ships (step 2).
3. **Slow subscribers versus retention:** retention wins, and the client is told with a `truncated` frame.
4. **Payload type:** strings stay. The record format can move to bytes later as its own change.
5. **Protocol versioning:** accept the break now; there are no external clients yet.
