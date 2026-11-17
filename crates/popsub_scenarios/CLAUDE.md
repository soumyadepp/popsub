# popsub_scenarios

The scenario suite every PopSub client library must pass. `popsub-scenarios --driver "<command>"` starts a real server per scenario and drives clients through driver processes speaking `clients/DRIVER.md`. `popsub-sdk-driver` is the Rust client's driver, and `tests/rust_sdk.rs` runs the whole suite against it in `make test`.

## Files

- `src/server.rs`: `ScenarioServer`. `start(Backend)` on a free port; `pause` (stop accepting, close every connection with `going_away`, keep the broker), `resume`, `restart` (a new broker over the reopened storage; accounts kept), `publish` from the server side, `last_seq`. Runs the broker's retry loop and stops it on restart and drop, since it holds the storage lock.
- `src/driver.rs`: `Driver`, one driver process: `send`, `ok`, `fails`, `event` (waits for a matching event, discarding others), `messages` (collects one topic's seqs). `connect_args` builds `connect` with fast reconnects.
- `src/scenarios.rs`: `SCENARIOS`, the list, and one function per scenario.
- `src/lib.rs`: `run`, one scenario at a time with a 60 second limit each.
- `src/main.rs`: the CLI (`--driver`, `--scenario`, `--list`).
- `src/bin/popsub-sdk-driver.rs`: the Rust client's driver.

## Rules

- A scenario checks behaviour the README in `clients/` promises, through the driver protocol only. If a scenario needs a new command or event, add it to `clients/DRIVER.md` and every driver.
- Before trusting a new scenario, break the behaviour in the Rust client and watch it fail.
- `messages` and `event` discard the events they don't want; take that into account when one scenario waits for several kinds.
- Scenario servers use `ack_timeout_ms = 300` and the retry loop runs once a second, so a resend comes 1 to 1.3 seconds after the original.

## Run

- `make scenarios` (the Rust client) or `make scenarios DRIVER="node clients/typescript/dist/driver.js"`.
