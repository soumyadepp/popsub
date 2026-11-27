# popsub_bench

`popsub-bench`, the load generator behind `docs/BENCHMARKS.md`. Each scenario starts a fresh release `popsub server` as a child process, drives it over raw WebSockets, and reports publishes and deliveries a second, delivery latency, and the CPU cores and memory the server used. `--target` runs the same loads against `nats-server` (core or JetStream) or `mosquitto`, each over its own protocol.

## Files

- `src/main.rs`: the CLI (`--target`, `--scenario`, `--list`, `--duration`, `--warmup`, `--payload`, `--backend`, `--server`, `--markdown`).
- `src/scenario.rs`: `scenarios()`, the list; `Group` and `Scenario`, which describe a load; `Target` and which scenarios it `supports`; `run_load` (a steady load for a fixed window) and `run_replay` (store, restart, read back).
- `src/load.rs`: everything that is the same for every broker. `pace` drives a publisher in three modes (`Mode::FlatOut`, `Mode::Rate`, `Mode::Confirmed`) through an `Outlet`; `SubscriberStats::delivery` counts and times a message; the `Clock`, the measurement `Window` and the payload `stamp`.
- `src/popsub.rs`, `src/nats.rs`, `src/mqtt.rs`: one wire each. An `Outlet` that writes a publish, a `publish` that runs `pace` and counts confirmations, and a `subscribe` that reads deliveries.
- `src/conn.rs`: PopSub connection setup: `login`, `open`, `subscribe`, `create_account`.
- `src/server.rs`: `ServerProcess` (start PopSub, NATS or Mosquitto; restart; kill on drop) and CPU and memory readings of a process.
- `src/report.rs`: the text table, the Markdown table and the notes.
- `src/tests.rs`: unit tests for the parts that don't need a server.

## Rules

- **Measure the real server.** The server is the release binary in its own process with a clean environment. Don't link the broker into the load generator, and don't go through `popsub_sdk`: its reconnection and deduplication would be measured too.
- **Say what differs from the defaults.** `ServerProcess::start` turns off the connection limit and puts the publish rate limit out of reach; `KEEP_EVERYTHING` raises retention so trimming stays out of the window. Any new deviation goes in `docs/BENCHMARKS.md` under "What differs from a default server". `one-topic-default-retention` is the scenario that runs with retention left alone.
- **Throughput is counted by arrival time** inside the window, never by the timestamp in the payload, so a backlog delivered late doesn't count.
- **Latency is only reported for paced or confirmed publishers.** A flat-out publisher fills the socket, so its delivery time is queueing in the load generator's own buffers.
- **Every broker gets the same pacing and the same measuring.** Anything about how a load is applied or counted goes in `load.rs`, never in a wire module. A wire module only translates.
- **A comparison scenario must mean the same thing on every broker.** `Target::supports` leaves out anything PopSub-specific. When a broker's defaults would lose messages or skew latency, change them in `ServerProcess` and list the change in `docs/BENCHMARKS.md`.
- **A scenario stops if the broker passes `MEMORY_LIMIT_MIB`.** A flat-out fan-out can make a broker buffer without bound, and the run must not exhaust the machine.
- **Check the load generator isn't the limit.** The `loadgen cores` column is there for that. A scenario where it is close to the cores the machine has measures the load generator.
- Benchmark topics and payloads contain no characters JSON escapes. `Incoming` borrows them, and acknowledgements write the topic back unescaped.
- One account per publisher, since the publish limiter is keyed by account. Subscribers share `bench-sub`.
- A new scenario gets a line in the table in `docs/BENCHMARKS.md`, and numbers in the docs are only replaced by a full run on one machine, never mixed.

## Run

- `make bench` builds the release binaries, raises the open file limit and runs everything (about six minutes).
- `make bench BENCH="--target nats"` runs the comparable scenarios against `nats-server`. The other targets are `nats-jetstream` and `mosquitto`; each needs its server installed.
- `make bench BENCH="--scenario one-topic --scenario fanout-100"` runs some. `target/release/popsub-bench --list` names them.
- Quit other heavy programs first. The load generator shares the machine with the server.
