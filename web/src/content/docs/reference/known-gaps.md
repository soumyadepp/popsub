---
title: Status and known gaps
description: What PopSub is today, what it deliberately isn't, and what's missing before it suits more uses, in priority order.
sidebar:
  order: 4
---

PopSub is a single process with a data directory. It doesn't cluster or replicate, so one
machine is the whole deployment. It's built for small applications that want durable,
realtime messaging with guarantees that are written down, and it's at version 0.x, so the
protocol and client APIs can still change. The
[changelog](https://github.com/soumyadepp/popsub/blob/main/CHANGELOG.md) says what changed in
each release.

It doesn't try to compete with Kafka or NATS on throughput, and it doesn't deliver exactly once:
every message carries its topic and `seq`, so consumers can spot repeats. If you need
replication today, use NATS JetStream or Kafka. If your devices only speak MQTT, use Mosquitto
or EMQX.

## What's missing

This is section 12 of
[`docs/DESIGN.md`](https://github.com/soumyadepp/popsub/blob/main/docs/DESIGN.md#12-known-gaps-in-priority-order),
most important first. The broker itself is in place. Most of the gaps are about getting
messages in and out, and about running it.

### Small applications

1. Your own backend can't issue PopSub tokens. Tokens only come from PopSub's login, signed with
   one shared secret.
2. There's no HTTP publish, so a serverless function has to open a WebSocket.
3. There are no storage or topic quotas per account, so one account can fill the disk.
4. Backups and upgrades aren't documented, and there's no promise yet that a new version reads
   an old data directory.

### Devices

5. No MQTT listener.
6. No last will, so a dashboard can't learn that a device dropped off.
7. No retained "current value" that every new subscriber gets automatically.
8. Logging in again needs the password on the device. No client certificates or per-device keys.
9. JSON, base64 and no compression are expensive on metered links.

### Running it

10. Capacity is unmeasured. Benchmarks are planned.
11. Power loss and a consumer that never reads aren't covered by tests yet.
12. A renewed TLS certificate needs a restart, and there's no built-in Let's Encrypt.
13. Metrics are pushed over OTLP. There's no Prometheus endpoint.
14. No `SECURITY.md`, and the frame parser isn't fuzzed.
15. No Swift, Kotlin, C or MicroPython client.

Where it's heading, clustering included, is in
[section 13](https://github.com/soumyadepp/popsub/blob/main/docs/DESIGN.md#13-proposed-direction).
