# Benchmarks

What one PopSub server does on one machine, measured with `popsub-bench` (`crates/popsub_bench`): before and after the 0.3.0 performance work, and beside NATS and Mosquitto under the same loads.

Every number on this page is from one run on one laptop, on 2026-11-27, with the load generator on the same machine. Read them as "roughly this, on this hardware". Where PopSub loses, the page says so.

## Summary

| What | 0.2.0 | Now |
|---|---:|---:|
| One topic, one publisher, one subscriber, QoS 0 | 33,000 messages a second | 1.9 million |
| The same with QoS 1 and acknowledgements | 25,000 | 369,000 |
| The same with the default retention | 118 | 1.8 million |
| QoS 1 with `synced` durability | 253 | 8,600 |
| Eight publishers on one topic | 45,000 | 2.3 million |
| Eight topics at once | 68,000 | 5.2 million |
| One topic fanned out to 100 subscribers | 554,000 deliveries a second | 13.7 million |
| Replaying stored history to one subscriber | 496,000 messages a second | 2.7 million |
| 10,000 idle connections | 1,027 MiB | 384 MiB |

Against the other brokers, on the same machine in the same run:

- **Ahead of Mosquitto** on every throughput test except 16 KiB payloads.
- **Ahead of NATS JetStream**, the comparable persistent broker, on one topic, 1 KiB payloads, eight publishers, two and eight topics, every fan-out and confirmed publishing. Behind it on four topics in this run, on 16 KiB payloads and on latency.
- **Ahead of NATS core**, which keeps nothing on disk, on eight publishers, eight topics and fan-out. Behind it on one topic (1.9 against 3.4 million), 1 KiB payloads, two and four topics, 16 KiB payloads and latency.
- **Less memory than NATS** with 10,000 idle connections: 384 MiB against 546 (core) and 603 (JetStream). Mosquitto needs 53.

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

- Apple M4 Pro, 14 cores (10 performance, 4 efficiency), 24 GB, macOS 27.0, on mains power
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
| `one-topic` | 32,772 | 1,901,314 | 58.0x | 1,542,922 | 3,360,014 | 339,923 |
| `one-topic-default-retention` | 118 | 1,765,645 | 14,963.1x |  |  |  |
| `payload-1k` | 30,917 | 1,190,916 | 38.5x | 1,173,586 | 2,548,942 | 315,459 |
| `payload-16k` | 17,466 | 18,499 | 1.1x | 256,789 | 306,809 | 156,588 |
| `fanin-8` | 44,520 | 2,267,515 | 50.9x | 1,461,050 | 1,939,848 | 346,136 |
| `topics-2` | 47,568 | 3,687,086 | 77.5x | 1,734,476 | 3,806,274 | 351,250 |
| `topics-4` | 62,086 | 1,688,791 | 27.2x | 2,060,028 | 4,467,720 | 349,150 |
| `topics-8` | 68,220 | 5,186,172 | 76.0x | 1,961,965 | 4,502,057 | 352,791 |
| `topics-1000` | 68,375 | 409,626 | 6.0x |  |  |  |
| `wildcard-100` | 66,688 | 661,328 | 9.9x |  |  |  |
| `quiet-beside-topics`, busy | 68,624 | 5,091,705 | 74.2x | 1,947,013 | 4,348,491 | 380,514 |

- `one-topic-default-retention`, `one-topic-memory`, `topics-1000` and `wildcard-100` only run on PopSub.
- `topics-4` came out at 1.7 million in this run. Run alone on a rested disk it gives 3.8 to 4.8 million; see [Known limits](#known-limits).

### Fan-out

Deliveries a second from one publisher to many subscribers, flat out.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `fanout-10` | 161,352 | 6,670,212 | 41.3x | 5,513,096 | 5,522,652 | 373,962 |
| `fanout-100` | 553,758 | 13,695,946 | 24.7x | 3,528,342 | 2,969,895 | 385,785 |
| `fanout-1000` | 657,212 | 3,604,472 | 5.5x | 0 | 0 | 378,532 |

At 1,000 subscribers only Mosquitto, the slowest, delivered everything. PopSub and both NATS modes lost deliveries, and JetStream also lost some at 100. See [Known limits](#known-limits).

### Confirmed publishing

Confirmed publishes a second with 64 in flight.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `one-topic-qos1` | 24,785 | 369,310 | 14.9x | 291,954 |  | 187,123 |
| `topics-8-qos1` | 61,830 | 330,474 | 5.3x | 325,303 |  | 185,970 |
| `one-topic-qos1-synced` | 253 | 8,618 | 34.1x |  |  |  |
| `shared-4` | 21,738 | 127,681 | 5.9x |  |  |  |

Delivery latency for the same rows, median and 99th percentile in milliseconds:

| Scenario | 0.2.0 | Now | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|
| `one-topic-qos1` | 2.48 / 3.49 | 0.14 / 0.28 | 0.06 / 0.10 |  | 1384.45 / 2310.14 |
| `topics-8-qos1` | 8.16 / 10.11 | 2.16 / 6.37 | 0.31 / 0.57 |  | 1368.06 / 2310.14 |
| `one-topic-qos1-synced` | 256.00 / 274.43 | 7.01 / 9.98 |  |  |  |
| `shared-4` | 2.61 / 9.92 | 0.41 / 7.01 |  |  |  |

### Latency at a fixed rate

Median and 99th percentile delivery time in milliseconds.

| Scenario | 0.2.0 | Now | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|
| `rate-1k` | 0.16 / 1.04 | 0.23 / 1.03 | 0.14 / 0.56 | 0.13 / 0.40 | 0.10 / 0.42 |
| `rate-10k` | 0.57 / 14.21 | 0.28 / 1.71 | 0.17 / 1.19 | 0.15 / 0.49 | 0.18 / 0.81 |
| `fanout-100-rate-1k` | 0.59 / 1.65 | 0.48 / 1.94 | 0.54 / 1.47 | 0.56 / 1.48 | 0.63 / 1.30 |
| `quiet-alone`, quiet | 0.27 / 1.01 | 0.27 / 0.79 | 0.17 / 0.50 | 0.14 / 0.38 | 0.12 / 0.27 |
| `quiet-beside-topics`, quiet | 0.11 / 0.21 | 0.84 / 8.89 | 0.07 / 0.22 | 0.09 / 0.95 | 0.80 / 1.33 |
| `quiet-beside-paced-fanout`, quiet | 4.13 / 5.41 | 3.60 / 4.54 | 25.34 / 68.09 | 26.11 / 55.81 | 2.33 / 6.40 |
| `quiet-beside-paced-fanout`, busy | 5.57 / 8.32 | 5.25 / 7.93 | 27.77 / 61.70 | 28.54 / 51.71 | 1.50 / 4.58 |
| `quiet-beside-fanout`, quiet | 6.01 / 15.04 | 0.24 / 190.46 | 0.08 / 0.59 | 0.04 / 0.24 | 57.09 / 133.12 |
| `idle-10000`, quiet | 0.16 / 4.35 | 0.28 / 6.43 | 0.16 / 6.08 | 0.15 / 5.47 | 0.13 / 5.66 |
| `slow-consumer`, fast | 0.42 / 2.46 | 0.26 / 1.13 |  |  |  |

- `rate-1k` and `rate-10k`: one topic at 1,000 and 10,000 messages a second.
- `quiet-*`: a topic at 200 messages a second, alone, beside eight flat-out topics, beside a 1,000 subscriber fan-out at 300 messages a second (both rows shown), and beside a flat-out one.
- `slow-consumer`: four subscribers at 2,000 a second while a fifth reads only 500.

### Replay

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `replay-200k` | 496,312 | 2,682,615 | 5.4x |  |  |  |

After a restart with 200,001 stored messages, PopSub accepted connections in 67 ms (88 ms on 0.2.0).

### Memory

Broker memory in MiB at the end of the scenario.

| Scenario | 0.2.0 | Now | Change | JetStream | NATS core | Mosquitto |
|---|---:|---:|---:|---:|---:|---:|
| `rate-1k` | 34 | 31 | 0.9x | 27 | 21 | 4 |
| `one-topic-default-retention` | 32 | 34 | 1.1x |  |  |  |
| `one-topic` | 121 | 159 | 1.3x | 146 | 27 | 4 |
| `fanin-8` | 150 | 1,086 | 7.2x | 147 | 28 | 4 |
| `fanout-100` | 65 | 140 | 2.2x | 1,684 | 1,140 | 4 |
| `idle-10000`, idle | 1,027 | 384 | 0.4x | 603 | 546 | 53 |

Opening, authenticating and subscribing 10,001 connections, 64 at a time, took PopSub 0.58 s (17,200 a second, 13,100 on 0.2.0), NATS 0.35 s and Mosquitto 0.34 s.

## Where PopSub loses

- **Large payloads.** At 16 KiB PopSub delivered 18,500 a second in this run, and 90,000 to 135,000 in runs on a rested disk; Mosquitto 157,000, JetStream 257,000, NATS core 307,000. PopSub hands every message to the operating system before delivering it, so a crash of the server never loses a message a subscriber saw. At 16 KiB that is 1.5 to 2 GB a second, more than this laptop's SSD sustains, so the operating system holds the writes up and delivery waits with them. A broker that delivers from memory and writes in the background keeps going, at the cost of losing its latest messages if it crashes.
- **Raw ingest against NATS core.** NATS core doesn't store messages. PopSub is 43% behind on one topic and about half on 1 KiB payloads.
- **Latency at low rates.** A median of 0.23 ms at 1,000 messages a second, against 0.10 to 0.14 ms for the others. The 99th percentile is 1.0 ms against 0.4 to 0.6.
- **A quiet topic beside busy ones.** Beside eight flat-out topics its median is 0.84 ms (0.07 to 0.09 for NATS), and its 99th percentile 8.9 ms. Beside a flat-out fan-out the 99th percentile is 190 ms. The server and the load generator together use 12 to 13 of the 14 cores there, but NATS stays fast under the same load, so this is a gap in PopSub.
- **Memory against Mosquitto.** 384 MiB for 10,000 idle connections against 53. Each retained record also costs about 50 bytes of memory in the `file` backend's index.
- **Opening connections.** About 17,000 a second against 29,000.

## Known limits

- **Results that write a lot to disk depend on the SSD's state.** Each scenario writes gigabytes, and an SSD slows down for a while after heavy writing even once it looks idle. The tool waits for a quiet disk before each scenario, but can't see that. Runs late in a full set, or right after 16 KiB payloads, can come out at a third of a run on a rested disk: `topics-4` gave 1.7 million here and 3.8 to 4.8 million in 16 runs on its own.
- **A flat-out fan-out to 1,000 subscribers drops connections on macOS.** PopSub writes faster than the load generator reads, the kernel runs out of network buffer space, and a write fails with `ENOBUFS`. The write may have been partly sent, so retrying it would corrupt the stream; PopSub closes the connection, and a client with a durable session resumes from the log. Both NATS modes lost deliveries in the same scenario. This hasn't been run on Linux.
- **`one-topic-memory` grows without bound.** With the `memory` backend and everything retained, memory grows by about a gigabyte every few seconds at full speed.
- **Many topics use a lot of CPU.** `topics-1000` and `wildcard-100` deliver 6 and 10 times what 0.2.0 did, but on 11 to 12 cores.
- **One machine.** Nothing is measured on Linux, over a real network, with TLS, with the load generator on a separate machine, or for longer than 8 seconds.

## What changed in 0.3.0

Each of these was found with a profile of the server or a measurement under one of the scenarios above.

| Change | What it removed |
|---|---|
| Publishes waiting for a topic are stored with one write | A write, and with `synced` a disk flush, per message |
| The connection doesn't wait for a publish to be stored | A round trip to the topic's writer per message |
| Retention trims the index in memory and writes the head file only when a segment empties | A disk flush per publish on a full topic |
| Records are stored in a compact binary form | About 40% of the bytes written to disk, and JSON parsing on replay |
| The `published` log line is at `debug` | Every thread queueing for the output lock |
| Each subscriber gets a batch in one go, and its frames are built once, outside the broker lock | A socket write per message per subscriber, and long holds of the lock |
| Delivery frames are written as JSON by hand | The largest CPU cost per message |
| A connection remembers that it may publish to a topic until an account changes | A lock every connection shared, taken per message |
| Topic writers run on their own threads | Writers competing with socket reads |
| The log is read a run of records at a time | Three system calls per record replayed |
| A 16 KiB WebSocket read buffer instead of 128 KiB, and no retry key in memory for server-assigned ids | About three quarters of the memory per idle connection, and almost half per retained record |

`CHANGELOG.md` lists what changed in behaviour.

## Full results

### PopSub now

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,901,296 | 1,901,314 | - | - | - | 2.37 | 159 | 1.23 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 369,310 | 369,310 | 0.14 | 0.28 | 4.13 | 2.54 | 205 | 1.07 |
| `one-topic-default-retention` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,765,587 | 1,765,645 | - | - | - | 2.37 | 34 | 1.17 |
| `one-topic-memory` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,797,732 | 1,797,729 | - | - | - | 2.84 | 3319 | 1.33 |
| `one-topic-qos1-synced` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 8,618 | 8,618 | 7.01 | 9.98 | 16.00 | 0.19 | 37 | 0.09 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,191,104 | 1,190,916 | - | - | - | 1.97 | 574 | 0.90 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 18,499 | 18,499 | - | - | - | 0.29 | 58 | 0.11 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 2,267,437 | 2,267,515 | - | - | - | 3.14 | 1086 | 1.48 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 666,968 | 6,670,212 | - | - | - | 2.91 | 350 | 3.66 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 137,405 | 13,695,946 | - | - | - | 2.79 | 140 | 10.05 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 264 | 3,604,472 | - | - | - | 1.41 | 404 | 1.98 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 3,687,186 | 3,687,086 | - | - | - | 4.77 | 165 | 2.72 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,688,419 | 1,688,791 | - | - | - | 2.55 | 625 | 1.79 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 5,185,543 | 5,186,172 | - | - | - | 7.60 | 273 | 5.15 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 330,474 | 330,461 | 2.16 | 6.37 | 13.38 | 9.30 | 108 | 1.32 |
| `topics-1000` | all | 1,000 topics, 8 publishers roaming, 1 subscriber each | QoS 0, flat out | 408,728 | 409,626 | - | - | - | 11.57 | 470 | 1.00 |
| `wildcard-100` | all | 100 topics, 4 publishers roaming, 1 wildcard subscriber | QoS 0, flat out | 661,504 | 661,328 | - | - | - | 10.48 | 364 | 0.66 |
| `shared-4` | all | 1 topic, 1 publisher, 4 subscribers sharing | QoS 1, 64 in flight | 127,681 | 127,678 | 0.41 | 7.01 | 14.65 | 3.25 | 75 | 1.19 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.23 | 1.03 | 16.25 | 0.14 | 31 | 0.07 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 10,000 | 10,000 | 0.28 | 1.71 | 11.97 | 0.25 | 38 | 0.11 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,012 | 0.48 | 1.94 | 4.38 | 1.15 | 36 | 0.98 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.27 | 0.79 | 11.58 | 0.05 | 31 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.24 | 190.46 | 272.38 | 2.44 | 352 | 5.07 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 16,272 | 7,979,879 | - | - | - | 2.44 | 352 | 5.07 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 3.60 | 4.54 | 5.21 | 4.16 | 71 | 3.38 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 300,002 | 5.25 | 7.93 | 9.92 | 4.16 | 71 | 3.38 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.84 | 8.89 | 16.51 | 7.59 | 257 | 5.01 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 5,093,061 | 5,091,705 | - | - | - | 7.59 | 257 | 5.01 |
| `slow-consumer` | fast | 1 topic, 1 publisher, 4 subscribers, 1 slow subscriber | QoS 0, 2000/s | 2,000 | 8,000 | 0.26 | 1.13 | 16.19 | 0.19 | 34 | 0.16 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.28 | 6.43 | 15.62 | 0.05 | 384 | 0.08 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.05 | 384 | 0.08 |
| `replay-200k` | all | 1 topic, 200,001 stored messages, 1 subscriber from seq 1 | store, then replay | 1,832,425 | 2,682,615 | - | - | - | 1.74 | 119 | 0.80 |

### PopSub 0.2.0

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 33,056 | 32,772 | - | - | - | 1.47 | 121 | 0.25 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 24,785 | 24,785 | 2.48 | 3.49 | 21.12 | 1.87 | 121 | 0.67 |
| `one-topic-default-retention` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 8 | 118 | - | - | - | 0.17 | 32 | 0.02 |
| `one-topic-memory` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 77,600 | 77,270 | - | - | - | 1.93 | 441 | 0.55 |
| `one-topic-qos1-synced` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 253 | 253 | 256.00 | 274.43 | 276.48 | 0.12 | 32 | 0.04 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 30,904 | 30,917 | - | - | - | 1.43 | 119 | 0.25 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 17,463 | 17,466 | - | - | - | 1.17 | 78 | 0.22 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 46,904 | 44,520 | - | - | - | 2.12 | 150 | 0.38 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 16,152 | 161,352 | - | - | - | 2.76 | 77 | 1.67 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 5,048 | 553,758 | - | - | - | 5.65 | 65 | 2.98 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 712 | 657,212 | - | - | - | 8.80 | 186 | 3.04 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 48,152 | 47,568 | - | - | - | 2.72 | 212 | 0.48 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 62,832 | 62,086 | - | - | - | 5.11 | 230 | 0.89 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 72,496 | 68,220 | - | - | - | 6.69 | 273 | 1.00 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 61,830 | 61,830 | 8.16 | 10.11 | 14.72 | 7.79 | 248 | 2.01 |
| `topics-1000` | all | 1,000 topics, 8 publishers roaming, 1 subscriber each | QoS 0, flat out | 69,536 | 68,375 | - | - | - | 6.77 | 513 | 1.02 |
| `wildcard-100` | all | 100 topics, 4 publishers roaming, 1 wildcard subscriber | QoS 0, flat out | 68,488 | 66,688 | - | - | - | 4.91 | 249 | 0.56 |
| `shared-4` | all | 1 topic, 1 publisher, 4 subscribers sharing | QoS 1, 64 in flight | 21,738 | 21,738 | 2.61 | 9.92 | 17.28 | 2.06 | 87 | 0.61 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.16 | 1.04 | 4.61 | 0.15 | 34 | 0.05 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 9,999 | 10,000 | 0.57 | 14.21 | 23.93 | 0.75 | 61 | 0.19 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,004 | 0.59 | 1.65 | 5.15 | 1.38 | 50 | 0.96 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.27 | 1.01 | 7.07 | 0.06 | 32 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 6.01 | 15.04 | 24.70 | 8.66 | 186 | 3.17 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 680 | 659,342 | - | - | - | 8.66 | 186 | 3.17 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 4.13 | 5.41 | 6.50 | 4.58 | 191 | 3.60 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 300,030 | 5.57 | 8.32 | 11.90 | 4.58 | 191 | 3.60 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.11 | 0.21 | 0.28 | 6.76 | 278 | 1.02 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 72,912 | 68,624 | - | - | - | 6.76 | 278 | 1.02 |
| `slow-consumer` | fast | 1 topic, 1 publisher, 4 subscribers, 1 slow subscriber | QoS 0, 2000/s | 2,000 | 8,000 | 0.42 | 2.46 | 10.37 | 0.53 | 43 | 0.28 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.16 | 4.35 | 10.37 | 0.03 | 1027 | 0.04 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.03 | 1027 | 0.04 |
| `replay-200k` | all | 1 topic, 200,001 stored messages, 1 subscriber from seq 1 | store, then replay | 37,186 | 496,312 | - | - | - | 1.59 | 162 | 0.35 |

### NATS JetStream

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,543,096 | 1,542,922 | - | - | - | 3.18 | 146 | 0.78 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 291,954 | 291,954 | 0.06 | 0.10 | 1.57 | 1.30 | 49 | 0.21 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 1,173,750 | 1,173,586 | - | - | - | 3.15 | 455 | 0.77 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 256,794 | 256,789 | - | - | - | 2.65 | 565 | 0.99 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 1,461,160 | 1,461,050 | - | - | - | 6.47 | 147 | 0.72 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 553,648 | 5,513,096 | - | - | - | 2.46 | 1082 | 7.68 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 37,664 | 3,528,342 | - | - | - | 4.79 | 1684 | 3.14 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 2,280,843 | 0 | - | - | - | 2.40 | 155 | 0.32 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,734,472 | 1,734,476 | - | - | - | 4.93 | 149 | 1.31 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 2,060,728 | 2,060,028 | - | - | - | 6.79 | 152 | 2.03 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,961,341 | 1,961,965 | - | - | - | 8.36 | 155 | 2.52 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 325,303 | 325,265 | 0.31 | 0.57 | 0.92 | 2.37 | 51 | 0.68 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.14 | 0.56 | 4.93 | 0.14 | 27 | 0.06 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 10,000 | 10,000 | 0.17 | 1.19 | 7.55 | 0.32 | 48 | 0.08 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 99,994 | 0.54 | 1.47 | 3.68 | 1.51 | 43 | 0.80 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.17 | 0.50 | 1.58 | 0.05 | 23 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.08 | 0.59 | 1.77 | 2.24 | 157 | 0.33 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 2,254,128 | 0 | - | - | - | 2.24 | 157 | 0.33 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 199 | 25.34 | 68.09 | 94.21 | 5.71 | 110 | 1.97 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 299,127 | 27.77 | 61.70 | 100.35 | 5.71 | 110 | 1.97 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.07 | 0.22 | 6.24 | 8.52 | 156 | 2.55 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 1,946,840 | 1,947,013 | - | - | - | 8.52 | 156 | 2.55 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.16 | 6.08 | 10.37 | 0.08 | 603 | 0.08 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.08 | 603 | 0.08 |

### NATS core

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 3,360,131 | 3,360,014 | - | - | - | 2.08 | 27 | 1.29 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 2,548,912 | 2,548,942 | - | - | - | 2.09 | 27 | 1.28 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 306,760 | 306,809 | - | - | - | 2.28 | 117 | 1.59 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 1,939,832 | 1,939,848 | - | - | - | 4.73 | 28 | 0.85 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 546,528 | 5,522,652 | - | - | - | 0.91 | 1050 | 8.53 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 33,744 | 2,969,895 | - | - | - | 5.68 | 1140 | 1.62 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 6,534,666 | 0 | - | - | - | 1.37 | 102 | 1.16 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 3,806,103 | 3,806,274 | - | - | - | 3.80 | 27 | 2.46 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,467,704 | 4,467,720 | - | - | - | 5.86 | 28 | 3.48 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,502,339 | 4,502,057 | - | - | - | 7.24 | 32 | 4.27 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.13 | 0.40 | 3.04 | 0.09 | 21 | 0.06 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 9,999 | 10,001 | 0.15 | 0.49 | 11.39 | 0.12 | 23 | 0.08 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 99,988 | 0.56 | 1.48 | 4.19 | 1.65 | 34 | 0.85 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.14 | 0.38 | 4.48 | 0.03 | 18 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.04 | 0.24 | 3.74 | 2.28 | 102 | 1.09 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 2,779,984 | 2,779,053 | - | - | - | 2.28 | 102 | 1.09 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 26.11 | 55.81 | 80.89 | 5.63 | 105 | 1.84 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 300,102 | 28.54 | 51.71 | 81.92 | 5.63 | 105 | 1.84 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.09 | 0.95 | 6.72 | 7.29 | 40 | 4.50 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 4,350,024 | 4,348,491 | - | - | - | 7.29 | 40 | 4.50 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.15 | 5.47 | 8.96 | 0.05 | 546 | 0.07 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.05 | 546 | 0.07 |

### Mosquitto

| Scenario | Group | Load | Mode | Publishes/s | Deliveries/s | p50 ms | p99 ms | Max ms | Server cores | Server MiB | Loadgen cores |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `one-topic` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 340,144 | 339,923 | - | - | - | 1.00 | 4 | 0.69 |
| `one-topic-qos1` | all | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 187,123 | 143,722 | 1384.45 | 2310.14 | 2326.53 | 1.00 | 254 | 0.85 |
| `payload-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 315,728 | 315,459 | - | - | - | 1.00 | 5 | 0.71 |
| `payload-16k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 156,589 | 156,588 | - | - | - | 1.00 | 4 | 0.81 |
| `fanin-8` | all | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 345,968 | 346,136 | - | - | - | 1.00 | 4 | 0.69 |
| `fanout-10` | all | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 37,640 | 373,962 | - | - | - | 1.00 | 4 | 1.62 |
| `fanout-100` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 4,000 | 385,785 | - | - | - | 1.00 | 4 | 1.68 |
| `fanout-1000` | all | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 472 | 378,532 | - | - | - | 1.00 | 9 | 1.68 |
| `topics-2` | all | 2 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 351,008 | 351,250 | - | - | - | 1.00 | 4 | 0.72 |
| `topics-4` | all | 4 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 349,688 | 349,150 | - | - | - | 1.00 | 4 | 0.72 |
| `topics-8` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 354,768 | 352,791 | - | - | - | 1.00 | 4 | 0.73 |
| `topics-8-qos1` | all | 8 topics, 1 publisher each, 1 subscriber each | QoS 1, 64 in flight | 185,970 | 143,018 | 1368.06 | 2310.14 | 2342.91 | 1.00 | 253 | 0.86 |
| `rate-1k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 1000/s | 1,000 | 1,000 | 0.10 | 0.42 | 7.62 | 0.03 | 4 | 0.06 |
| `rate-10k` | all | 1 topic, 1 publisher, 1 subscriber | QoS 0, 10000/s | 9,999 | 9,999 | 0.18 | 0.81 | 10.30 | 0.15 | 4 | 0.15 |
| `fanout-100-rate-1k` | all | 1 topic, 1 publisher, 100 subscribers | QoS 0, 1000/s | 1,000 | 100,000 | 0.63 | 1.30 | 5.05 | 0.37 | 4 | 0.68 |
| `quiet-alone` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.12 | 0.27 | 2.62 | 0.01 | 4 | 0.04 |
| `quiet-beside-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 57.09 | 133.12 | 142.34 | 1.00 | 9 | 1.71 |
| `quiet-beside-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 440 | 384,642 | - | - | - | 1.00 | 9 | 1.71 |
| `quiet-beside-paced-fanout` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 2.33 | 6.40 | 9.79 | 0.78 | 9 | 1.40 |
| `quiet-beside-paced-fanout` | busy | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, 300/s | 300 | 299,953 | 1.50 | 4.58 | 7.33 | 0.78 | 9 | 1.40 |
| `quiet-beside-topics` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.80 | 1.33 | 1.70 | 1.00 | 4 | 0.77 |
| `quiet-beside-topics` | busy | 8 topics, 1 publisher each, 1 subscriber each | QoS 0, flat out | 379,888 | 380,514 | - | - | - | 1.00 | 4 | 0.77 |
| `idle-10000` | quiet | 1 topic, 1 publisher, 1 subscriber | QoS 0, 200/s | 200 | 200 | 0.13 | 5.66 | 12.35 | 0.01 | 53 | 0.07 |
| `idle-10000` | idle | 1 topic, no publisher, 10,000 subscribers | idle | 0 | 0 | - | - | - | 0.01 | 53 | 0.07 |

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
- Quit other heavy programs first, plug the machine in, keep it awake, and run the full set on a rested disk. The load generator and the broker share the machine.

A pull request that claims a performance change should show these numbers before and after, from the same machine.
