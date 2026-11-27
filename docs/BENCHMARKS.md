# Benchmarks

What one PopSub server does on one machine, measured with `popsub-bench` (`crates/popsub_bench`): before and after the 0.3.0 performance work, and beside NATS and Mosquitto under the same loads.

Every number on this page is from one run on one laptop, on 2026-11-27, with the load generator on the same machine. Read them as "roughly this, on this hardware". Where PopSub loses, the page says so.

## Summary

| What | 0.2.0 | Now |
|---|---:|---:|
| One topic, one publisher, one subscriber, QoS 0 | 33,000 messages a second | 2.0 million |
| The same with QoS 1 and acknowledgements | 23,000 | 395,000 |
| The same with the default retention | 116 | 1.6 million |
| QoS 1 with `synced` durability | 258 | 9,300 |
| Eight publishers on one topic | 44,000 | 1.9 million |
| One topic fanned out to 100 subscribers | 540,000 deliveries a second | 11 million |
| Replaying stored history to one subscriber | 483,000 messages a second | 1.6 million |

Against the other brokers, on the same machine in the same run:

- **Ahead of Mosquitto** on every throughput test except 16 KiB payloads.
- **Ahead of NATS JetStream**, the comparable persistent broker, on one topic, eight publishers, two and four topics, every fan-out and confirmed publishing. Behind it on eight topics in this run and on 16 KiB payloads.
- **Behind NATS core**, which keeps nothing on disk, on every ingest test: by 4% to 8% on eight publishers and on two and four topics, by 37% on one topic. Ahead of it on fan-out.
- **Behind all three** on 16 KiB payloads, on delivery latency at low rates, and on memory.

[Where PopSub loses](#where-popsub-loses) and [Known limits](#known-limits) have the detail.

## How it is measured

- Each scenario starts a fresh broker process with its own data directory. For PopSub that is a release build of `popsub server`.
- The load generator speaks each broker's own protocol over raw sockets: PopSub's JSON over WebSocket, the NATS text protocol, and MQTT 3.1.1. It doesn't use a client library.
- Traffic runs for 2 seconds of warm-up, then 8 seconds are measured. Payloads are 128 bytes unless a scenario says otherwise.
- **Publishes a second** counts publishes sent inside the window. For confirmed publishing it counts the confirmations that arrived inside it.
- **Deliveries a second** counts messages that arrived at subscribers inside the window, added up over all subscribers.
- **Latency** is the time from a publish being sent to it arriving at a subscriber. It is only reported for publishers that are paced or wait for confirmation. A flat-out publisher fills its own socket, so its latency would mostly be time queued in the load generator.
- **Server cores** is CPU time the broker used divided by the window's length. **Loadgen cores** is the same for the load generator.
- Before each scenario the tool waits for the disk to go quiet (a `sync`, then until it writes less than 20 MB/s, for at most a minute). A scenario can write several gigabytes, and without the wait the next one measures the disk catching up.
- After the window, publishers stop and the run waits for deliveries still on their way. A scenario that doesn't receive everything is listed under [Known limits](#known-limits).

### The machine

- Apple M4 Pro, 14 cores (10 performance, 4 efficiency), 24 GB, macOS 27.0, on battery
- PopSub: the `file` backend with `buffered` durability, on the internal SSD
- NATS: `nats-server` 2.15.3. JetStream uses a file stream with one subject per topic
- Mosquitto: 2.1.4

### What differs from a default server

| Broker | Setting | Benchmark value | Why |
|---|---|---|---|
| PopSub | `limits.publish_per_second`, `limits.publish_burst` | Out of reach | The default is 100 publishes a second per account |
| PopSub | `broker.max_connections` | No limit | The default is 1,000 |
| PopSub | `storage.retention.*` | Keeps everything for the run | So trimming isn't part of what a scenario measures. `one-topic-default-retention` leaves it alone |
| Mosquitto | `persistence` | Off | Its default |
| Mosquitto | `max_queued_messages` | No limit | The default drops QoS 1 messages past 1,000 queued |
| Mosquitto | `set_tcp_nodelay` | On | Without it, latency at low rates is dominated by a 40 ms delay |
| NATS | none | | Started with its defaults |

Each PopSub publisher has its own account, since the publish limiter is keyed by account. Subscribers share one.

### What "the same load" means

| | PopSub | NATS core | NATS JetStream | Mosquitto |
|---|---|---|---|---|
| QoS 0 publish | Stored in the topic log, then delivered | Delivered, never stored | Stored in a stream, then delivered | Delivered, never stored |
| Confirmed publish | QoS 1: `published` after the append | Not offered | Publish acknowledgement from the stream | QoS 1: `PUBACK` |
| Subscriber acknowledgement | `ack` per message | Not offered | Not used: a push consumer with no acknowledgement | `PUBACK` per message |

So NATS core and Mosquitto do less work for each QoS 0 message than PopSub and JetStream, which write it to disk.

## Results

### Publishing and delivering, flat out

Deliveries a second, QoS 0, publishers sending as fast as the broker reads.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `one-topic` | 33,203 | 2,046,349 | 61.6x | 1,541,488 | 3,259,422 | 338,968 |
| `one-topic-default-retention` | 116 | 1,633,436 | 14,081.3x |  |  |  |
| `payload-1k` | 30,991 | 1,281,287 | 41.3x | 1,174,656 | 2,474,300 | 321,012 |
| `payload-16k` | 17,958 | 44,635 | 2.5x | 257,095 | 305,566 | 159,263 |
| `fanin-8` | 44,378 | 1,856,065 | 41.8x | 1,462,308 | 1,931,889 | 339,584 |
| `topics-2` | 48,159 | 3,488,550 | 72.4x | 1,701,712 | 3,798,427 | 366,378 |
| `topics-4` | 63,551 | 4,118,047 | 64.8x | 2,036,950 | 4,447,193 | 352,092 |
| `topics-8` | 70,507 | 1,806,962 | 25.6x | 1,988,941 | 4,416,690 | 375,144 |
| `topics-1000` | 70,542 | 415,844 | 5.9x |  |  |  |
| `wildcard-100` | 67,002 | 484,777 | 7.2x |  |  |  |
| `quiet-beside-topics`, busy | 70,175 | 4,994,575 | 71.2x | 1,957,029 | 4,297,548 | 384,243 |

- `one-topic-default-retention`, `topics-1000` and `wildcard-100` only run on PopSub.
- `topics-8` swung between 1.3 million and 4.7 million from run to run. The last row is the same eight topics beside one quiet topic, in the same run. See [Known limits](#known-limits).

### Fan-out

Deliveries a second from one publisher to many subscribers, flat out.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `fanout-10` | 163,210 | 6,380,137 | 39.1x | 5,792,802 | 4,361,674 | 375,684 |
| `fanout-100` | 540,218 | 11,039,214 | 20.4x | 2,187,904 | 3,730,468 | 388,774 |
| `fanout-1000` | 645,637 | 4,284,500 | 6.6x | 0 | 499,946 | 375,827 |

At 1,000 subscribers only Mosquitto, the slowest, delivered everything. PopSub and both NATS modes lost deliveries. See [Known limits](#known-limits).

### Confirmed publishing

Confirmed publishes a second with 64 in flight.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `one-topic-qos1` | 22,596 | 394,560 | 17.5x | 292,877 |  | 179,839 |
| `topics-8-qos1` | 63,216 | 326,104 | 5.2x | 314,545 |  | 186,159 |
| `one-topic-qos1-synced` | 258 | 9,289 | 36.0x |  |  |  |
| `shared-4` | 21,916 | 124,950 | 5.7x |  |  |  |

Delivery latency for the same rows, median and 99th percentile in milliseconds:

| Scenario | 0.2.0 | Now | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|
| `one-topic-qos1` | 2.70 / 4.16 | 0.14 / 0.20 | 0.06 / 0.10 |  | 1622.02 / 2310.14 |
| `topics-8-qos1` | 8.00 / 9.73 | 2.14 / 6.17 | 0.32 / 0.56 |  | 1433.60 / 2473.98 |
| `one-topic-qos1-synced` | 243.71 / 303.10 | 6.82 / 10.05 |  |  |  |
| `shared-4` | 2.59 / 10.05 | 0.42 / 7.71 |  |  |  |

### Latency at a fixed rate

Median and 99th percentile delivery time in milliseconds.

| Scenario | 0.2.0 | Now | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|
| `rate-1k` | 0.17 / 1.50 | 0.22 / 1.05 | 0.13 / 0.35 | 0.12 / 0.32 | 0.10 / 0.21 |
| `rate-10k` | 0.49 / 1.81 | 0.27 / 0.83 | 0.16 / 0.79 | 0.14 / 0.57 | 0.17 / 0.48 |
| `fanout-100-rate-1k` | 0.51 / 1.52 | 0.49 / 2.25 | 0.54 / 1.35 | 0.52 / 1.55 | 0.66 / 1.68 |
| `quiet-alone`, quiet | 0.22 / 0.54 | 0.23 / 0.74 | 0.13 / 0.36 | 0.13 / 0.31 | 0.10 / 0.22 |
| `quiet-beside-topics`, quiet | 0.12 / 0.23 | 0.85 / 12.99 | 0.07 / 0.19 | 0.09 / 0.67 | 0.80 / 1.42 |
| `quiet-beside-paced-fanout`, quiet | 3.74 / 5.31 | 3.47 / 5.41 | 23.68 / 63.74 | 25.34 / 68.09 | 1.92 / 6.66 |
| `quiet-beside-paced-fanout`, busy | 5.21 / 8.45 | 4.86 / 8.13 | 25.98 / 50.69 | 27.26 / 61.95 | 1.42 / 5.50 |
| `quiet-beside-fanout`, quiet | 5.92 / 16.06 | 0.24 / 195.58 |  | 0.04 / 0.95 | 58.88 / 129.53 |
| `idle-10000`, quiet | 0.23 / 5.63 | 0.24 / 6.43 | 0.13 / 5.86 | 0.12 / 4.48 | 0.11 / 6.30 |
| `slow-consumer`, fast | 0.36 / 1.68 | 0.25 / 1.52 |  |  |  |

- `rate-1k` and `rate-10k`: one topic at 1,000 and 10,000 messages a second.
- `quiet-*`: a topic at 200 messages a second, alone, beside eight flat-out topics, beside a 1,000 subscriber fan-out at 300 messages a second (both rows shown), and beside a flat-out one.
- `slow-consumer`: four subscribers at 2,000 a second while a fifth reads only 500.

### Replay

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `replay-200k` | 482,680 | 1,633,417 | 3.4x |  |  |  |

After a restart with 200,001 stored messages, PopSub accepted connections in 101 ms (89 ms on 0.2.0).

### Memory

Broker memory in MiB at the end of the scenario.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `rate-1k` | 34 | 32 | 0.9x | 28 | 21 | 4 |
| `one-topic-default-retention` | 31 | 35 | 1.1x |  |  |  |
| `one-topic` | 122 | 1,746 | 14.3x | 145 | 27 | 4 |
| `fanin-8` | 150 | 1,653 | 11.0x | 146 | 28 | 4 |
| `fanout-100` | 66 | 190 | 2.9x | 1,335 | 1,712 | 4 |
| `idle-10000`, idle | 1,465 | 1,482 | 1.0x | 553 | 546 | 53 |

Opening, authenticating and subscribing 10,001 connections, 64 at a time, took PopSub 0.58 s (17,300 a second), NATS 0.33 s and Mosquitto 0.31 s.

## Where PopSub loses

- **Large payloads.** At 16 KiB PopSub delivered 45,000 a second in this run, and up to 130,000 in others; Mosquitto 159,000, JetStream 257,000, NATS core 306,000. A PopSub message is JSON text in a WebSocket frame: it is unmasked, checked as UTF-8, parsed, written to the log with a checksum and written out as JSON again, and each of those passes over the payload.
- **Raw ingest against NATS core.** NATS core doesn't store messages. PopSub is 4% behind on eight publishers and 37% behind on one topic.
- **Latency at low rates.** A median of 0.22 ms at 1,000 messages a second, against 0.10 to 0.13 ms for the others. The 99th percentile is 1.05 ms against 0.21 to 0.35 ms.
- **A quiet topic beside busy ones.** Beside eight flat-out topics its median went from 0.12 ms on 0.2.0 to 0.85 ms, and its 99th percentile from 0.23 ms to 13 ms. The server is doing 70 times the work there, but the quiet topic pays for it. Beside a flat-out fan-out the 99th percentile is 196 ms.
- **Memory.** With every record retained, PopSub held 1.7 GiB after 20 million messages on one topic, because the `file` backend keeps an index entry in memory for each retained record. With the default retention the same load used 35 MiB. With 10,000 idle connections it held 1.5 GiB, the same as 0.2.0, against about 550 MiB for NATS and 53 MiB for Mosquitto.
- **Opening connections.** About 17,000 a second against 30,000.

## Known limits

- **The numbers on this page were measured before two later fixes.** The run-to-run swing (eight topics between 1.3 and 4.7 million a second) came from the benchmark: each scenario started while the disk was still writing the gigabytes the previous one left. The tool now waits for a quiet disk, and records are stored in a smaller format; with both, eight topics gives about 4.5 million a second back to back. An SSD also slows down after tens of gigabytes of writes, so run the full set on a rested machine.
- **16 KiB payloads are limited by the disk.** PopSub hands every message to the operating system before delivering it, so a crash of the server never loses a message a subscriber saw. When the disk falls behind, the operating system holds those writes up, and delivery waits with them. A broker that delivers from memory and writes in the background keeps going, at the cost of losing its latest messages if it crashes.
- **A flat-out fan-out to 1,000 subscribers drops connections on macOS.** The broker can now write faster than the load generator reads, the kernel runs out of socket buffer space, and a write fails with `ENOBUFS`. PopSub closes that connection, because the write may have been partly sent. NATS core and JetStream lost deliveries in the same scenario. A client that reconnects with a durable session resumes from the log. This hasn't been run on Linux.
- **`one-topic-memory` stopped early.** With the `memory` backend and everything retained, the server passed the benchmark's 6 GiB limit within the window: it now stores messages faster than the run can hold in memory.
- **Many topics use a lot of CPU.** `topics-1000` and `wildcard-100` deliver 6 to 7 times what 0.2.0 did, but on 11 to 12 cores.
- **One machine, one run, on battery.** Nothing is measured on Linux, over a real network, with TLS, or for longer than 8 seconds.

## What changed in 0.3.0

Each of these was found with a profile of the server under one of the scenarios above.

| Change | What it removed |
|---|---|
| Publishes waiting for a topic are stored with one write | A write, and with `synced` a disk flush, per message |
| The connection doesn't wait for a publish to be stored | A round trip to the topic's writer per message |
| Retention trims the index in memory and writes the head file only when a segment empties | A disk flush per publish on a full topic |
| The `published` log line is at `debug` | Every thread queueing for the output lock |
| Each subscriber gets a batch in one go, and its frames are built once, outside the broker lock | A socket write per message per subscriber, and long holds of the lock |
| Records and frames are written as JSON by hand | The largest CPU cost per message |
| A connection remembers that it may publish to a topic until an account changes | A lock every connection shared, taken per message |
| Topic writers run on their own threads | Writers competing with socket reads |
| The log is read a run of records at a time | Three system calls per record replayed |

`CHANGELOG.md` lists what changed in behaviour.

## Full results

### PopSub now

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 2,046,248 | 2,046,349 | - | - | - | 2.33 | 1746 | 1.29 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 394,560 | 394,560 | 0.14 | 0.20 | 8.96 | 2.55 | 367 | 1.08 |
| `one-topic-default-retention` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,633,680 | 1,633,436 | - | - | - | 1.88 | 35 | 0.94 |
| `one-topic-qos1-synced` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 9,289 | 9,289 | 6.82 | 10.05 | 17.54 | 0.18 | 41 | 0.09 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,281,282 | 1,281,287 | - | - | - | 2.15 | 866 | 0.96 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 44,645 | 44,635 | - | - | - | 0.62 | 102 | 0.25 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 1,855,912 | 1,856,065 | - | - | - | 2.27 | 1653 | 1.15 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 637,853 | 6,380,137 | - | - | - | 2.80 | 483 | 3.48 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 109,440 | 11,039,214 | - | - | - | 2.45 | 190 | 8.18 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 2,208 | 4,284,500 | - | - | - | 1.75 | 446 | 2.53 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 3,488,262 | 3,488,550 | - | - | - | 4.50 | 122 | 2.62 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,119,547 | 4,118,047 | - | - | - | 6.36 | 873 | 4.62 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,806,993 | 1,806,962 | - | - | - | 2.51 | 721 | 1.44 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 326,104 | 326,105 | 2.14 | 6.17 | 35.58 | 9.43 | 204 | 1.30 |
| `topics-1000` | all | 1,000 topics, 8 publishers roaming, 1 subscriber each | QoS 0, flat out | 416,864 | 415,844 | - | - | - | 11.74 | 736 | 0.98 |
| `wildcard-100` | all | 100 topics, 4 publishers roaming, 1 wildcard subscriber | QoS 0, flat out | 485,112 | 484,777 | - | - | - | 9.56 | 474 | 0.51 |
| `shared-4` | all | 1 topic, 1 publisher, 4 subscribers sharing | QoS 1, 64 in flight | 124,950 | 124,949 | 0.42 | 7.71 | 15.04 | 3.19 | 141 | 1.13 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.22 | 1.05 | 6.72 | 0.13 | 32 | 0.07 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 10,000 | 10,000 | 0.27 | 0.83 | 10.43 | 0.26 | 42 | 0.11 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 99,995 | 0.49 | 2.25 | 6.30 | 1.26 | 48 | 1.07 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.23 | 0.74 | 8.83 | 0.04 | 31 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.24 | 195.58 | 266.24 | 1.12 | 383 | 2.92 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 5,840 | 4,044,464 | - | - | - | 1.12 | 383 | 2.92 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 3.47 | 5.41 | 10.56 | 4.23 | 184 | 3.70 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 300,043 | 4.86 | 8.13 | 15.94 | 4.23 | 184 | 3.70 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.85 | 12.99 | 37.63 | 7.99 | 465 | 4.85 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,995,632 | 4,994,575 | - | - | - | 7.99 | 465 | 4.85 |
| `slow-consumer` | fast | 1 topic, 1 publisher, 4 subscribers, 1 slow subscriber | QoS 0, 2000/s | 2,000 | 8,000 | 0.25 | 1.52 | 16.64 | 0.19 | 38 | 0.16 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.24 | 6.43 | 15.10 | 0.04 | 1482 | 0.07 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.04 | 1482 | 0.07 |
| `replay-200k` | all | 1 topic, 200,001 stored messages, 1 subscriber from seq 1 | store, then replay | 2,063,255 | 1,633,417 | - | - | - | 1.31 | 164 | 0.49 |

### PopSub 0.2.0

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 33,896 | 33,203 | - | - | - | 1.48 | 122 | 0.25 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 22,596 | 22,596 | 2.70 | 4.16 | 20.35 | 1.86 | 118 | 0.65 |
| `one-topic-default-retention` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 8 | 116 | - | - | - | 0.14 | 31 | 0.01 |
| `one-topic-memory` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 81,160 | 81,335 | - | - | - | 1.96 | 458 | 0.56 |
| `one-topic-qos1-synced` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 258 | 258 | 243.71 | 303.10 | 307.20 | 0.12 | 32 | 0.04 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 30,984 | 30,991 | - | - | - | 1.43 | 119 | 0.24 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 17,959 | 17,958 | - | - | - | 1.25 | 79 | 0.23 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 45,608 | 44,378 | - | - | - | 2.10 | 150 | 0.38 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 16,664 | 163,210 | - | - | - | 2.75 | 78 | 1.69 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 5,520 | 540,218 | - | - | - | 5.80 | 66 | 2.99 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 712 | 645,637 | - | - | - | 8.91 | 187 | 3.31 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 48,080 | 48,159 | - | - | - | 2.71 | 213 | 0.49 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 63,104 | 63,551 | - | - | - | 5.16 | 226 | 0.90 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 73,944 | 70,507 | - | - | - | 7.09 | 275 | 1.04 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 63,216 | 63,216 | 8.00 | 9.73 | 14.85 | 8.08 | 258 | 2.10 |
| `topics-1000` | all | 1,000 topics, 8 publishers roaming, 1 subscriber each | QoS 0, flat out | 71,304 | 70,542 | - | - | - | 7.17 | 515 | 1.05 |
| `wildcard-100` | all | 100 topics, 4 publishers roaming, 1 wildcard subscriber | QoS 0, flat out | 69,136 | 67,002 | - | - | - | 4.94 | 247 | 0.55 |
| `shared-4` | all | 1 topic, 1 publisher, 4 subscribers sharing | QoS 1, 64 in flight | 21,916 | 21,916 | 2.59 | 10.05 | 15.10 | 2.06 | 88 | 0.61 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.17 | 1.50 | 13.76 | 0.16 | 34 | 0.06 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 10,000 | 10,000 | 0.49 | 1.81 | 10.18 | 0.73 | 62 | 0.17 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,000 | 0.51 | 1.52 | 10.30 | 1.46 | 50 | 0.99 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.22 | 0.54 | 6.08 | 0.05 | 31 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 5.92 | 16.06 | 25.09 | 8.40 | 189 | 3.17 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 680 | 645,163 | - | - | - | 8.40 | 189 | 3.17 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 3.74 | 5.31 | 8.13 | 4.74 | 196 | 3.91 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 299,961 | 5.21 | 8.45 | 15.55 | 4.74 | 196 | 3.91 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.12 | 0.23 | 2.22 | 7.11 | 273 | 1.05 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 72,048 | 70,175 | - | - | - | 7.11 | 273 | 1.05 |
| `slow-consumer` | fast | 1 topic, 1 publisher, 4 subscribers, 1 slow subscriber | QoS 0, 2000/s | 2,000 | 8,001 | 0.36 | 1.68 | 8.70 | 0.56 | 43 | 0.30 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.23 | 5.63 | 13.18 | 0.05 | 1465 | 0.07 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.05 | 1465 | 0.07 |
| `replay-200k` | all | 1 topic, 200,001 stored messages, 1 subscriber from seq 1 | store, then replay | 37,992 | 482,680 | - | - | - | 1.57 | 162 | 0.34 |

### NATS JetStream

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,541,168 | 1,541,488 | - | - | - | 3.21 | 145 | 0.81 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 292,877 | 292,872 | 0.06 | 0.10 | 1.56 | 1.29 | 49 | 0.21 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,174,976 | 1,174,656 | - | - | - | 3.19 | 457 | 0.80 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 257,118 | 257,095 | - | - | - | 3.28 | 517 | 0.99 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 1,462,200 | 1,462,308 | - | - | - | 6.58 | 146 | 0.72 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 585,392 | 5,792,802 | - | - | - | 2.15 | 1090 | 8.17 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 23,744 | 2,187,904 | - | - | - | 4.50 | 1335 | 1.18 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 2,370,536 | 0 | - | - | - | 2.24 | 153 | 0.32 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,701,759 | 1,701,712 | - | - | - | 4.98 | 147 | 1.30 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 2,036,448 | 2,036,950 | - | - | - | 6.93 | 150 | 2.05 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,988,065 | 1,988,941 | - | - | - | 8.61 | 154 | 2.60 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 314,545 | 314,545 | 0.32 | 0.56 | 0.83 | 2.34 | 53 | 0.67 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.13 | 0.35 | 2.27 | 0.14 | 28 | 0.06 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 9,999 | 9,999 | 0.16 | 0.79 | 14.02 | 0.32 | 47 | 0.08 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,000 | 0.54 | 1.35 | 3.18 | 1.54 | 43 | 0.82 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.13 | 0.36 | 8.51 | 0.04 | 23 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 0 | 0 | - | - | - | 4.17 | 5910 | 1.44 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 39,768 | 701,416 | - | - | - | 4.17 | 5910 | 1.44 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 23.68 | 63.74 | 95.23 | 5.83 | 109 | 2.41 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 299,632 | 25.98 | 50.69 | 90.11 | 5.83 | 109 | 2.41 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.07 | 0.19 | 0.59 | 8.56 | 159 | 2.57 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,957,608 | 1,957,029 | - | - | - | 8.56 | 159 | 2.57 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.13 | 5.86 | 7.68 | 0.06 | 553 | 0.07 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.06 | 553 | 0.07 |

### NATS core

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 3,259,528 | 3,259,422 | - | - | - | 2.07 | 27 | 1.16 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 2,474,749 | 2,474,300 | - | - | - | 2.12 | 29 | 1.28 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 305,555 | 305,566 | - | - | - | 2.24 | 119 | 1.58 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 1,932,128 | 1,931,889 | - | - | - | 4.83 | 28 | 0.86 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 462,232 | 4,361,674 | - | - | - | 0.99 | 1083 | 8.01 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 37,960 | 3,730,468 | - | - | - | 5.87 | 1712 | 3.82 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 3,384 | 499,946 | - | - | - | 2.37 | 3290 | 0.71 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 3,798,335 | 3,798,427 | - | - | - | 3.86 | 27 | 2.34 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,447,286 | 4,447,193 | - | - | - | 5.96 | 28 | 3.85 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,417,476 | 4,416,690 | - | - | - | 7.35 | 34 | 4.56 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.12 | 0.32 | 12.35 | 0.10 | 21 | 0.07 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 9,999 | 9,999 | 0.14 | 0.57 | 30.85 | 0.12 | 23 | 0.08 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,006 | 0.52 | 1.55 | 12.99 | 1.56 | 34 | 0.80 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.13 | 0.31 | 8.26 | 0.03 | 18 | 0.03 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.04 | 0.95 | 4.74 | 1.20 | 104 | 0.85 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 6,903,448 | 0 | - | - | - | 1.20 | 104 | 0.85 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 25.34 | 68.09 | 98.81 | 5.85 | 105 | 2.14 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 299,559 | 27.26 | 61.95 | 109.06 | 5.85 | 105 | 2.14 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.09 | 0.67 | 6.88 | 7.40 | 35 | 4.54 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,298,609 | 4,297,548 | - | - | - | 7.40 | 35 | 4.54 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.12 | 4.48 | 8.26 | 0.04 | 546 | 0.06 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.04 | 546 | 0.06 |

### Mosquitto

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 339,240 | 338,968 | - | - | - | 1.00 | 4 | 0.68 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 179,839 | 138,134 | 1622.02 | 2310.14 | 2342.91 | 1.00 | 248 | 0.86 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 321,320 | 321,012 | - | - | - | 1.00 | 5 | 0.70 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 159,264 | 159,263 | - | - | - | 1.00 | 4 | 0.82 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 341,592 | 339,584 | - | - | - | 1.00 | 4 | 0.69 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 37,696 | 375,684 | - | - | - | 1.00 | 4 | 1.62 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 3,824 | 388,774 | - | - | - | 1.00 | 4 | 1.68 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 472 | 375,827 | - | - | - | 1.00 | 9 | 1.67 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 366,568 | 366,378 | - | - | - | 1.00 | 4 | 0.74 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 352,224 | 352,092 | - | - | - | 1.00 | 4 | 0.74 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 376,928 | 375,144 | - | - | - | 1.00 | 4 | 0.75 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 186,159 | 143,130 | 1433.60 | 2473.98 | 2490.37 | 1.00 | 272 | 0.86 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.10 | 0.21 | 7.33 | 0.03 | 4 | 0.05 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 10,000 | 10,000 | 0.17 | 0.48 | 4.25 | 0.15 | 4 | 0.15 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,000 | 0.66 | 1.68 | 2.35 | 0.38 | 4 | 0.71 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.10 | 0.22 | 1.76 | 0.01 | 4 | 0.03 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 58.88 | 129.53 | 158.72 | 1.00 | 9 | 1.78 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 440 | 405,685 | - | - | - | 1.00 | 9 | 1.78 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 1.92 | 6.66 | 12.48 | 0.76 | 9 | 1.39 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 300,007 | 1.42 | 5.50 | 9.47 | 0.76 | 9 | 1.39 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.80 | 1.42 | 1.70 | 1.00 | 4 | 0.77 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 384,576 | 384,243 | - | - | - | 1.00 | 4 | 0.77 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.11 | 6.30 | 14.14 | 0.01 | 53 | 0.06 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.01 | 53 | 0.06 |

## Run it yourself

```bash
make bench
make bench BENCH="--scenario one-topic --scenario fanout-100"
make bench BENCH="--target nats-jetstream"
target/release/popsub-bench --list
```

- `make bench` builds the release binaries, raises the open file limit and runs every scenario. It takes about five minutes.
- `--target nats`, `nats-jetstream` or `mosquitto` runs the comparable scenarios against that broker, which must be installed.
- `--server path/to/popsub` measures another build, such as the previous release.
- `--duration`, `--warmup` and `--payload` change the window and payload size. `--backend memory` runs PopSub on the memory backend.
- `--markdown results.md` also writes the results as a Markdown table.
- Quit other heavy programs first, plug the machine in and keep it awake. The load generator and the broker share the machine.

A pull request that claims a performance change should show these numbers before and after, from the same machine.
