HOST ?= 127.0.0.1
PORT ?= 8080
URL ?= ws://$(HOST):$(PORT)
CRATE ?=
JOBS ?=
DATA_DIR ?= pubsub_data

CARGO ?= cargo
JOBS_FLAG := $(if $(JOBS),-j $(JOBS),)
SERVER_ENV := SERVER_HOST=$(HOST) SERVER_PORT=$(PORT)

.DEFAULT_GOAL := help

.PHONY: help setup build release server server-release client demo \
	test test-crate scenarios fmt fmt-check lint ci schema schema-check doc watch coverage reset-data clean

help:
	@printf "PopSub make targets\n\n"
	@printf "  Getting started\n"
	@printf "    make setup           Install rustfmt, clippy and the pre-commit hooks\n"
	@printf "    make demo            Start a server, run the client against it, stop\n\n"
	@printf "  Running\n"
	@printf "    make server          Run the broker (HOST=$(HOST) PORT=$(PORT))\n"
	@printf "    make server-release  Run an optimised build of the broker\n"
	@printf "    make client          Run the CLI client against URL=$(URL)\n"
	@printf "    make watch           Rebuild and restart the server on every change (cargo-watch)\n\n"
	@printf "  Quality\n"
	@printf "    make test            Run every test in the workspace\n"
	@printf "    make test-crate      Run one crate's tests, e.g. CRATE=popsub_broker\n"
	@printf "    make scenarios       Run the client scenario suite against DRIVER (default: the Rust client)\n"
	@printf "    make fmt             Format the code\n"
	@printf "    make lint            Clippy with warnings as errors\n"
	@printf "    make ci              Exactly what GitHub Actions runs: fmt check, clippy, tests, schema check\n"
	@printf "    make schema          Regenerate docs/protocol/*.json from the protocol types\n"
	@printf "    make coverage        Coverage report (cargo-llvm-cov)\n\n"
	@printf "  Housekeeping\n"
	@printf "    make build           Debug build\n"
	@printf "    make release         Release build\n"
	@printf "    make doc             Build and open the API docs\n"
	@printf "    make reset-data      Delete stored messages and sessions ($(DATA_DIR), and the old pubsub_db)\n"
	@printf "    make clean           cargo clean plus reset-data\n\n"
	@printf "  Options: HOST, PORT, URL, CRATE, DRIVER, JOBS (parallel build jobs, lower it if builds run out of memory)\n"

setup:
	@command -v $(CARGO) >/dev/null || { echo "Rust is not installed. See https://rustup.rs"; exit 1; }
	rustup component add rustfmt clippy
	@if command -v pre-commit >/dev/null; then pre-commit install; \
	else echo "pre-commit not found, skipping hooks (pip install pre-commit to enable them)"; fi

build:
	$(CARGO) build --workspace $(JOBS_FLAG)

release:
	$(CARGO) build --workspace --release $(JOBS_FLAG)

server:
	$(SERVER_ENV) $(CARGO) run -p popsub $(JOBS_FLAG) -- server

server-release:
	$(SERVER_ENV) $(CARGO) run -p popsub --release $(JOBS_FLAG) -- server

client:
	$(CARGO) run -p popsub $(JOBS_FLAG) -- client --url $(URL)

demo: build
	@set -e; \
	if nc -z $(HOST) $(PORT) 2>/dev/null; then echo "Port $(PORT) is already in use. Try make demo PORT=8090"; exit 1; fi; \
	$(SERVER_ENV) ./target/debug/popsub server & pid=$$!; \
	trap 'kill $$pid 2>/dev/null' EXIT INT TERM; \
	for i in $$(seq 1 50); do nc -z $(HOST) $(PORT) 2>/dev/null && break; sleep 0.2; done; \
	nc -z $(HOST) $(PORT) || { echo "Server did not start on $(HOST):$(PORT)"; exit 1; }; \
	./target/debug/popsub client --url $(URL)

test:
	$(CARGO) test --workspace $(JOBS_FLAG)

test-crate:
	@test -n "$(CRATE)" || { echo "Usage: make test-crate CRATE=popsub_broker"; exit 1; }
	$(CARGO) test -p $(CRATE) $(JOBS_FLAG)

DRIVER ?= target/debug/popsub-sdk-driver

scenarios:
	$(CARGO) build -p popsub_scenarios $(JOBS_FLAG)
	target/debug/popsub-scenarios --driver "$(DRIVER)"

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

lint:
	$(CARGO) clippy --workspace --all-targets --all-features $(JOBS_FLAG) -- -D warnings

ci: fmt-check lint test schema-check

schema:
	POPSUB_UPDATE_SCHEMA=1 $(CARGO) test -p popsub_protocol --features schema $(JOBS_FLAG) schema_files_match_the_types

schema-check:
	$(CARGO) test -p popsub_protocol --features schema $(JOBS_FLAG)

doc:
	$(CARGO) doc --workspace --no-deps --open

watch:
	@command -v cargo-watch >/dev/null || { echo "Install it first: cargo install cargo-watch"; exit 1; }
	$(SERVER_ENV) $(CARGO) watch -x "run -p popsub -- server"

coverage:
	@command -v cargo-llvm-cov >/dev/null || { echo "Install it first: cargo install cargo-llvm-cov"; exit 1; }
	$(CARGO) llvm-cov --workspace --html --open

reset-data:
	rm -rf $(DATA_DIR) pubsub_db

clean: reset-data
	$(CARGO) clean
