# Contributing to PopSub

Thanks for helping. This guide covers how to get set up, how to make a change that will be accepted, and what each kind of change has to update along the way.

## Before you start

- Read **How it works** in the [README](README.md). The five ideas there explain most of the code.
- Read [`docs/DESIGN.md`](docs/DESIGN.md) before changing behaviour. It describes the protocol and the delivery guarantees exactly as the code provides them. [`docs/STORAGE.md`](docs/STORAGE.md) covers storage.
- Read [`docs/STYLE.md`](docs/STYLE.md) before writing code. The style rules are enforced in review.
- For anything bigger than a small fix (a protocol change, a change to delivery guarantees, a new backend), open an issue first describing the problem and the approach, so we can agree on it before you write it.

## Setting up

You need Rust 1.89 or newer.

```bash
git clone https://github.com/soumyadepp/popsub.git
cd popsub
make setup        # rustfmt, clippy, and the pre-commit hooks if pre-commit is installed
make test         # every test in the workspace
make demo         # start a server, run the client against it, stop
```

If a build runs out of memory, add `JOBS=2` to any target. If port 8080 is taken, use `make demo PORT=8090`.

To work on a client library you also need its toolchain: Node 22 or newer for TypeScript, [uv](https://docs.astral.sh/uv/) with Python 3.10 or newer for Python, and Go 1.23 or newer for Go. Each client's README lists its commands. `make scenarios` runs the shared client scenario suite against the Rust client, and `make scenarios DRIVER="..."` against another.

## Making a change

1. **Branch from `main`.** Name it for what it does: `feat/...`, `fix/...`, `docs/...` or `chore/...`.
2. **Keep each pull request to one concern.** A refactor and a behaviour change are two pull requests. When one change depends on another that isn't merged yet, stack them: one pull request per step, each based on the one before, with the merge order in the description.
3. **Write the failing test first** for a bug fix or a behaviour change, then make it pass. Tests should check behaviour (what a client sees, what the server stores), not implementation details.
4. **Run the checks** before pushing. `make ci` runs what the Rust CI workflow runs: the format check, clippy with warnings as errors, every test, and the protocol schema check. A change to a client library also runs that client's own checks and the scenario suite against it in CI (`.github/workflows/clients.yml`); run them locally first.

## What each kind of change must also update

| If you change... | Also... |
|---|---|
| The protocol: any frame, field, error code or close code | Change the types in `popsub_protocol` and its exact-JSON tests, run `make schema` and commit `docs/protocol/`, update `docs/DESIGN.md` section 4 and the WebSocket tests, and the README's example session if it shows that frame. The TypeScript, Python and Go clients write their protocol types by hand, so update them too. Within a protocol version, only add things: never remove, rename or change the meaning of a field, code or frame. |
| Delivery behaviour: ordering, acknowledgements, retries, replay, sessions | Add a test that fails without your change, and update the guarantees table in `docs/DESIGN.md` section 6 and the README's **Delivery guarantees**. Never describe a guarantee more strongly than the code provides it. |
| A setting | Add the field and its default in `popsub_config/src/settings.rs`, add it to `config/default.toml`, and document it in `docs/DESIGN.md` section 9. Security-related defaults stay closed (registration off, new users with no topics). |
| Storage behaviour | Update `docs/STORAGE.md`. A new rule every backend must follow goes into the conformance suite in `popsub_storage/src/conformance.rs`. |
| A storage backend | Run the conformance suite against it with `conformance_tests!`; every check must pass. |
| How a client behaves (anything in `clients/README.md`) | Add a scenario to `crates/popsub_scenarios/src/scenarios.rs` that fails without the change, and update every client library and its driver so `make scenarios` passes for each. |
| A metric | Add it in `popsub_utils::metrics` and to the table in `docs/DESIGN.md` section 3.3. |
| A new crate | Give it `publish = false` unless it is meant for crates.io (see `docs/RELEASING.md`). |
| Anything a user would notice | Add a line to `CHANGELOG.md` under the next version. |
| A crate's structure or invariants | Update that crate's `CLAUDE.md`. These files are the maintainers' notes on each crate (and what coding assistants read), so they must stay accurate. |

## Code style

The full rules are in [`docs/STYLE.md`](docs/STYLE.md). In short:

- Every `.rs` file starts with a `//!` module docstring, and every function, type, constant and test has a `///` docstring in Google style, with `Args:`, `Returns:` and `Raises:` sections where they apply.
- No `//` comments. If something needs explaining, the explanation goes in the docstring of the item it belongs to.
- Write plain, specific English: say what the code does and why. No em or en dashes, no marketing words.
- `cargo fmt` and `cargo clippy --all-targets --all-features -- -D warnings` must pass.
- Don't `unwrap()` on a path that handles client input. Log with `tracing` and structured fields.
- In the broker, take the lock only through `SharedBroker::with` (never across an `.await`), and do storage I/O only in the topic writer.

Before you push, this should print nothing:

```bash
grep -rnE --include='*.rs' '^\s*//[^/!]' crates
```

## Pull requests

- **Title:** what the change does, in the imperative: "Add per-topic retention overrides".
- **Description:**
  - **Why:** the problem.
  - **What changes:** the approach, and anything a reviewer should know, such as a breaking change or a merge order.
  - **How it was tested:** commands you ran, and numbers where they matter. A claim that something is faster needs before and after measurements and how they were taken.
- **Commits:** an imperative subject line of 72 characters or fewer, and a body that explains why when it isn't obvious.
- **CI must pass.** Reviews look for the things in this guide first.

## Reporting bugs

Open an issue with:
- the commit or version
- your settings (without secrets)
- the steps to reproduce
- what you expected and what happened
- the server's log around the time

For a protocol problem, the frames sent and received are the most useful thing you can include.

## Fuzzing

The parsers that read raw network input have fuzz targets in `fuzz/`. Run them after changing `popsub_protocol`'s client frames, `parse_frame` or the PROXY header reader.

You need nightly Rust and cargo-fuzz:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz --locked
```

Then run a target until you stop it, or for a set time:

```bash
make fuzz FUZZ_TARGET=client_frame
make fuzz FUZZ_TARGET=proxy_header FUZZ_SECONDS=300
```

| Target | What it checks |
|---|---|
| `client_frame` | `parse_frame` never panics and gives the same result as the general `ClientFrame` parser, and `salvage_request_id` never panics |
| `proxy_header` | `read_proxy_header` never panics on any bytes, and consumes bytes whenever it reads a header |

A crash is saved under `fuzz/artifacts/<target>/`. Turn it into a unit test that fails before you fix the bug.

## Security issues

Please don't open a public issue for a vulnerability. [`SECURITY.md`](SECURITY.md) says how to report one privately, what to include, and what happens next.

## License

By contributing, you agree that your contributions are licensed under the Apache License 2.0, the same as the project.
