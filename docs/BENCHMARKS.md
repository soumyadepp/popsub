# Benchmarks

What one PopSub server does on one machine, measured with `popsub-bench` (`crates/popsub_bench`), and what limits it today.

These numbers are from one laptop, with the load generator on the same machine. Read them as "roughly this, on this hardware", and as a baseline to compare a change against. They are not a comparison with other brokers; see [What isn't measured](#what-isnt-measured). The table lists the scenarios that existed when it was measured; `popsub-bench --list` names them all.

## Summary

Measured on 2026-11-27, on `main` after 0.2.0, with the default `file` backend unless a row says otherwise.

| What | Result |
|---|---|
| One topic, one publisher, one subscriber, QoS 0 | 33,000 messages a second |
| The same with QoS 1 and acknowledgements | 24,000 a second |
| Eight topics at once | 72,000 a second in total |
| One topic fanned out to 1,000 subscribers | 664,000 deliveries a second |
| Delivery latency at 1,000 messages a second | 0.23 ms median, 2.1 ms at the 99th percentile |
| Delivery latency for 100 subscribers at 1,000 a second | 0.58 ms median, 4.8 ms at the 99th percentile |
| Replaying stored history to one subscriber | 463,000 messages a second |
| Restart with 200,000 stored messages | Accepting connections after 96 ms |
| 10,000 idle subscribed connections | 341 MiB, about 31 KiB each |
| Opening, authenticating and subscribing connections | About 15,000 a second |

The run also found four limits that matter more than those numbers. They are described under [What limits it today](#what-limits-it-today):

1. **With the default retention, a busy topic drops to about 110 messages a second** once it holds 1,000 messages.
2. **Every publish writes a log line**, and all threads queue for the output lock to write it. This is what stops throughput growing with more topics.
3. **A topic with many subscribers delays every other topic** while it fans out, because fan-out runs under the one broker lock.
4. **`synced` durability does one disk sync per publish**: 257 messages a second on this machine.

## How it is measured

- Each scenario starts a fresh release build of `popsub server` as its own process, with its own data directory.
- The load generator opens raw WebSocket connections and speaks the protocol directly. It doesn't use `popsub_sdk`, so the client library's reconnection and deduplication aren't measured.
- Traffic runs for 2 seconds of warm-up, then 8 seconds are measured. Payloads are 128 bytes unless a scenario says otherwise.
- **Publishes a second** counts publishes sent inside the window. For QoS 1 it counts `published` replies that arrived inside it.
- **Deliveries a second** counts messages that arrived at subscribers inside the window, added up over all subscribers.
- **Latency** is the time from a publish being sent to it arriving at a subscriber. The send time travels in the payload. It is only reported for publishers that are paced or wait for confirmation. A flat-out publisher fills its own socket, so its latency would mostly be time spent queued in the load generator.
- **Server cores** is CPU time the server used divided by the window's length. **Loadgen cores** is the same for the load generator, to show it wasn't the limit.
- After the window, publishers stop and the run waits for deliveries still on their way. A scenario that doesn't receive everything is listed under Problems.

### What differs from a default server

| Setting | Benchmark value | Why |
|---|---|---|
| `limits.publish_per_second`, `limits.publish_burst` | Out of reach | The default is 100 publishes a second per account |
| `broker.max_connections` | No limit | The default is 1,000 |
| `storage.retention.*` | Keeps everything for the run | So trimming isn't part of what a scenario measures. `one-topic-default-retention` leaves it alone |

Each publisher has its own account, since the publish limiter is keyed by account. Subscribers share one.

## Results

- Machine: Apple M4 Pro, 14 cores (10 performance, 4 efficiency), 24 GB, macOS 27.0
- Storage backend: `file` with `buffered` durability, on the internal SSD
- Window: 8 s measured after 2 s of warm-up
- Payload: 128 bytes

### Throughput

| Scenario | Load | Mode | Publishes/s | Deliveries/s | Server cores | Server MiB | Loadgen cores |
|---|---|---|---:|---:|---:|---:|---:|
| `one-topic` | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 33,069 | 32,367 | 1.46 | 119 | 0.25 |
| `one-topic-qos1` | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 23,894 | 23,894 | 1.84 | 120 | 0.66 |
| `one-topic-default-retention` | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | blocked | 114 | 0.14 | 31 | 0.01 |
| `one-topic-memory` | 1 topic, 1 publisher, 1 subscriber | QoS 0, flat out | 75,808 | 75,805 | 1.93 | 428 | 0.54 |
| `one-topic-qos1-synced` | 1 topic, 1 publisher, 1 subscriber | QoS 1, 64 in flight | 257 | 257 | 0.09 | 32 | 0.03 |
| `payload-1k` | 1 topic, 1 KiB payloads | QoS 0, flat out | 30,960 | 30,956 | 1.43 | 117 | 0.24 |
| `payload-16k` | 1 topic, 16 KiB payloads | QoS 0, flat out | 16,830 | 16,834 | 1.21 | 77 | 0.22 |
| `fanin-8` | 1 topic, 8 publishers, 1 subscriber | QoS 0, flat out | 46,008 | 43,072 | 2.09 | 149 | 0.37 |
| `fanout-10` | 1 topic, 1 publisher, 10 subscribers | QoS 0, flat out | 16,792 | 162,066 | 2.73 | 77 | 1.67 |
| `fanout-100` | 1 topic, 1 publisher, 100 subscribers | QoS 0, flat out | 6,072 | 551,397 | 5.38 | 65 | 2.92 |
| `fanout-1000` | 1 topic, 1 publisher, 1,000 subscribers | QoS 0, flat out | 712 | 663,951 | 8.35 | 187 | 3.22 |
| `topics-2` | 2 topics, 1 publisher and 1 subscriber each | QoS 0, flat out | 45,736 | 44,730 | 2.74 | 150 | 0.49 |
| `topics-4` | 4 topics, 1 publisher and 1 subscriber each | QoS 0, flat out | 65,400 | 63,714 | 5.17 | 232 | 0.91 |
| `topics-8` | 8 topics, 1 publisher and 1 subscriber each | QoS 0, flat out | 72,016 | 70,138 | 6.98 | 266 | 1.03 |
| `topics-8-qos1` | 8 topics, 1 publisher and 1 subscriber each | QoS 1, 64 in flight | 62,124 | 62,124 | 7.99 | 246 | 2.02 |
| `topics-1000` | 1,000 topics, 8 roaming publishers, 1 subscriber each | QoS 0, flat out | 69,072 | 69,392 | 7.03 | 513 | 1.02 |
| `wildcard-100` | 100 topics, 4 roaming publishers, 1 wildcard subscriber | QoS 0, flat out | 68,192 | 66,648 | 4.90 | 247 | 0.55 |
| `shared-4` | 1 topic, 1 publisher, 4 subscribers in a shared group | QoS 1, 64 in flight | 20,799 | 20,799 | 2.06 | 85 | 0.60 |
| `replay-200k` | 200,001 stored messages, 1 subscriber from `seq` 1 | Store, then replay | 33,516 | 463,498 | 1.60 | 162 | 0.35 |

In `one-topic-default-retention` the publisher spends the window blocked on a full socket, so the deliveries column is the rate the server actually sustains. It is also the one scenario that didn't finish draining.

In `replay-200k`, publishes a second is the rate the messages were stored at, and deliveries a second is the rate they were read back.

### Latency

Delivery time in milliseconds, from publish to arrival.

| Scenario | Load | Rate | p50 | p99 | Max |
|---|---|---|---:|---:|---:|
| `rate-1k` | 1 topic, 1 subscriber | 1,000/s | 0.23 | 2.11 | 13.82 |
| `rate-10k` | 1 topic, 1 subscriber | 10,000/s | 0.56 | 2.17 | 11.97 |
| `fanout-100-rate-1k` | 1 topic, 100 subscribers | 1,000/s | 0.58 | 4.83 | 44.03 |
| `one-topic-qos1` | 1 topic, 1 subscriber, QoS 1 | 23,894/s | 2.48 | 4.70 | 160.77 |
| `topics-8-qos1` | 8 topics, QoS 1 | 62,124/s | 8.10 | 10.30 | 25.73 |
| `shared-4` | 4 workers sharing, QoS 1 | 20,799/s | 2.65 | 10.18 | 19.20 |
| `one-topic-qos1-synced` | 1 topic, QoS 1, `synced` | 257/s | 244.74 | 325.63 | 327.68 |

The QoS 1 rows run with 64 publishes in flight, so their latency includes waiting behind up to 63 others.

### Isolation

A quiet topic at 200 messages a second, on its own and beside other load. Latency is for the quiet topic.

| Scenario | What else is running | p50 | p99 | Max |
|---|---|---:|---:|---:|
| `quiet-alone` | Nothing | 0.20 | 0.97 | 4.19 |
| `quiet-beside-topics` | 8 topics flat out, 64,000 messages a second | 0.13 | 2.01 | 9.79 |
| `quiet-beside-fanout` | 1 topic fanning out to 1,000 subscribers, 663,000 deliveries a second | 5.76 | 15.10 | 22.14 |
| `idle-10000` | 10,000 idle subscribed connections | 0.30 | 3.71 | 11.84 |
| `slow-consumer` | Measured on 4 subscribers at 2,000/s while a fifth reads only 500/s | 0.38 | 3.92 | 20.86 |

### Notes from the run

- Opening, authenticating and subscribing 10,001 connections took 0.68 s, 64 at a time: about 14,750 a second.
- After a restart with 200,001 stored messages, the server accepted connections in 96 ms.
- With 10,000 idle connections the server held 341 MiB ten seconds after they connected, against 32 MiB with two. A shorter run measured it four seconds after connecting and saw 1,180 MiB, so memory right after a burst of connections is higher than at rest.

## What limits it today

Each of these is a finding of this run, not a design goal.

### 1. Default retention makes a busy topic crawl

| | Deliveries a second |
|---|---:|
| Retention raised (`one-topic`) | 32,367 |
| Default retention (`one-topic-default-retention`) | 114 |

- By default a topic keeps 1,000 messages. Once it holds that many, every publish trims the oldest one.
- Each trim rewrites the topic's small head file and syncs it to disk. On this machine a sync takes about 9 ms, so the topic manages about 110 publishes a second.
- A disk sync is usually slower on macOS than on a Linux server, so the drop should be smaller there. It will still be there.
- The same happens when the age or size limit starts trimming.

### 2. Every publish writes a log line

- The server logs one `INFO` line per publish, and the level can't be changed.
- A profile of the server during `topics-8` (on the memory backend) found about 7,700 samples where a thread was waiting for a lock. About 7,150 of them were waiting for the standard output lock. About 210 were waiting for the broker lock, and about 310 for the memory store's lock.
- It is the largest single thing threads wait on, and the likeliest reason eight topics do 2.2 times the work of one while using 4.8 times the CPU.

### 3. A large fan-out delays other topics

- Delivering to a topic's subscribers happens while holding the one lock that guards all broker state. The frame is serialised again for each subscriber inside it.
- `quiet-beside-fanout` shows the cost: the quiet topic's median latency goes from 0.20 ms to 5.76 ms, and its 99th percentile from 0.97 ms to 15.10 ms.
- Busy CPUs alone don't explain it. `quiet-beside-topics` uses nearly as many cores with no fan-out, and the quiet topic is unaffected.

### 4. `synced` durability syncs once per publish

- 257 confirmed publishes a second, with a median wait of 245 ms at 64 in flight.
- Publishes that arrive together aren't grouped into one sync.

### Smaller things

- The memory backend is 2.3 times as fast as the file backend on one topic (75,800 against 33,100 a second), though neither syncs. The file backend hands every append to a blocking thread.
- Fan-out is limited by one publisher's rate falling as subscribers grow: 16,800 publishes a second at 10 subscribers, 712 at 1,000.

## Run it yourself

```bash
make bench
make bench BENCH="--scenario one-topic --scenario fanout-100"
target/release/popsub-bench --list
```

- `make bench` builds the release binaries, raises the open file limit and runs every scenario. It takes about six minutes.
- `--duration`, `--warmup` and `--payload` change the window and payload size. `--backend memory` runs everything on the memory backend.
- `--markdown results.md` also writes the results as a Markdown table.
- Quit other heavy programs first. The load generator and the server share the machine.

A pull request that claims a performance change should show these numbers before and after, from the same machine.

## What isn't measured

- **Other brokers.** `popsub-bench --target nats`, `nats-jetstream` or `mosquitto` runs the comparable scenarios against those brokers on the same machine, but no results are published here yet.
- **Linux.** Disk syncs and thread scheduling differ, so the storage numbers in particular will change.
- **A real network.** Everything runs over loopback, with no TLS.
- **Long runs.** Eight seconds doesn't show memory growth, retention under steady load, or session expiry.
- **Durable sessions and reconnection.** Subscribers here have no session id and never drop.
- **Failure.** Nothing is killed mid-run except in the restart before the replay, which is a clean stop.
- **Client libraries.** The load generator bypasses them.
