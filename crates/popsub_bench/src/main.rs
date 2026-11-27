//! `popsub-bench`: measures a PopSub server's throughput and delivery latency.
//!
//! Each scenario starts a fresh `popsub server` process, opens raw WebSocket connections to
//! it, applies a load for a fixed window and reports publishes and deliveries a second,
//! delivery latency, and how many CPU cores the server and the load generator used.

mod conn;
mod load;
mod mqtt;
mod nats;
mod popsub;
mod report;
mod scenario;
mod server;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use crate::scenario::{RunOptions, Scenario, ScenarioResult, Target, run, scenarios};
use crate::server::{Backend, find_program};

/// Command-line options.
#[derive(Debug, Parser)]
#[command(
    name = "popsub-bench",
    about = "Measure a PopSub server's throughput and latency"
)]
struct Options {
    /// The broker to measure.
    #[arg(long, value_enum, default_value = "popsub")]
    target: Target,

    /// Path to the broker's executable. Defaults to `popsub` beside this program, or to
    /// `nats-server` or `mosquitto` wherever they are installed.
    #[arg(long)]
    server: Option<PathBuf>,

    /// Storage backend the server runs with.
    #[arg(long, value_enum, default_value = "file")]
    backend: Backend,

    /// Seconds measured in each scenario.
    #[arg(long, default_value_t = 8)]
    duration: u64,

    /// Seconds of traffic before measuring starts.
    #[arg(long, default_value_t = 2)]
    warmup: u64,

    /// Payload size in bytes.
    #[arg(long, default_value_t = 128)]
    payload: usize,

    /// Run only this scenario; repeat to run several.
    #[arg(long = "scenario")]
    only: Vec<String>,

    /// List the scenarios and exit.
    #[arg(long)]
    list: bool,

    /// Also write the results as Markdown to this file.
    #[arg(long)]
    markdown: Option<PathBuf>,
}

/// Finds the executable of the broker to measure.
///
/// Args:
///     target: The broker.
///     given: The `--server` option, if it was passed.
///
/// Returns:
///     The path given. Without one: `popsub` in the directory this program runs from, or
///     `nats-server` or `mosquitto` from `PATH` and the usual install folders.
///
/// Raises:
///     String: When it doesn't exist, with how to get it.
fn server_binary(target: Target, given: Option<PathBuf>) -> Result<PathBuf, String> {
    let (path, hint) = match target {
        Target::Popsub => (
            std::env::current_exe()
                .map_err(|e| e.to_string())?
                .with_file_name("popsub"),
            "Build it with `cargo build --release -p popsub`",
        ),
        Target::Nats | Target::NatsJetstream => (
            find_program("nats-server").unwrap_or_else(|| PathBuf::from("nats-server")),
            "Install it, for example with `brew install nats-server`",
        ),
        Target::Mosquitto => (
            find_program("mosquitto").unwrap_or_else(|| PathBuf::from("mosquitto")),
            "Install it, for example with `brew install mosquitto`",
        ),
    };
    let path = given.unwrap_or(path);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "no {} executable at {}. {hint}, or pass --server.",
            target.label(),
            path.display()
        ))
    }
}

/// Picks the scenarios to run.
///
/// Args:
///     only: The names passed with `--scenario`. Empty means all of them.
///
/// Returns:
///     The chosen scenarios, in their usual order.
///
/// Raises:
///     String: When a name matches no scenario.
fn chosen(only: &[String]) -> Result<Vec<Scenario>, String> {
    let all = scenarios();
    for name in only {
        if !all.iter().any(|scenario| scenario.name == name) {
            return Err(format!("no scenario named {name}. Use --list to see them."));
        }
    }
    Ok(all
        .into_iter()
        .filter(|scenario| only.is_empty() || only.iter().any(|name| name == scenario.name))
        .collect())
}

/// Runs the chosen scenarios and prints the results.
///
/// Args:
///     options: The parsed command line.
///
/// Returns:
///     Every scenario's measurements.
///
/// Raises:
///     String: When the options are invalid or a scenario can't be set up.
async fn bench(options: Options) -> Result<Vec<ScenarioResult>, String> {
    let mut chosen = chosen(&options.only)?;
    let target = options.target;
    chosen.retain(|scenario| {
        let supported = target.supports(scenario);
        if !supported {
            eprintln!(
                "skipping {}: not comparable on {}",
                scenario.name,
                target.label()
            );
        }
        supported
    });
    let run_options = RunOptions {
        target,
        binary: server_binary(target, options.server)?,
        backend: options.backend,
        warmup: Duration::from_secs(options.warmup),
        duration: Duration::from_secs(options.duration.max(1)),
        payload_bytes: options.payload,
    };
    eprintln!("machine: {}", report::machine());
    eprintln!("target:  {}", target.label());
    eprintln!("server:  {}", run_options.binary.display());
    let mut results = Vec::new();
    for scenario in &chosen {
        server::settle_disk();
        eprintln!("running {}: {}", scenario.name, scenario.about);
        results.push(run(scenario, &run_options).await?);
    }
    print!("{}", report::text_table(&results));
    for line in report::notes(&results) {
        println!("note: {line}");
    }
    for line in report::problems(&results) {
        eprintln!("warning: {line}");
    }
    if let Some(path) = &options.markdown {
        std::fs::write(path, report::markdown(&results, &run_options))
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    Ok(results)
}

/// Parses the command line and runs the benchmark.
///
/// Returns:
///     Success when every scenario ran, failure when one couldn't be set up.
#[tokio::main]
async fn main() -> ExitCode {
    let options = Options::parse();
    if options.list {
        for scenario in scenarios() {
            println!("{:<28} {}", scenario.name, scenario.about);
        }
        return ExitCode::SUCCESS;
    }
    match bench(options).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
