//! The scenario suite every PopSub client library must pass.
//!
//! Each scenario starts a real server (`server`), which it can pause,
//! restart and publish to directly, and drives one or more clients through
//! driver processes (`driver`) speaking the protocol in `clients/DRIVER.md`.
//! `run` runs the scenarios in `scenarios` against one driver command.

pub mod driver;
pub mod scenarios;
pub mod server;

use std::time::{Duration, Instant};

/// How long one scenario may take before it fails.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(60);

/// One scenario's result.
#[derive(Debug, Clone)]
pub struct Report {
    /// The scenario's name.
    pub name: &'static str,
    /// How long it took.
    pub took: Duration,
    /// Why it failed, or `None` when it passed.
    pub failure: Option<String>,
}

/// Runs scenarios one after another against a driver.
///
/// Args:
///     driver: The shell command that starts the driver.
///     only: Names to run; empty runs every scenario.
///
/// Returns:
///     A report per scenario run, in order.
pub async fn run(driver: &str, only: &[String]) -> Vec<Report> {
    run_reporting(driver, only, |_| {}).await
}

/// Runs scenarios as `run` does, handing each report over as soon as its
/// scenario finishes.
///
/// Args:
///     driver: The shell command that starts the driver.
///     only: Names to run; empty runs every scenario.
///     on_report: Called with each report as it is made.
///
/// Returns:
///     A report per scenario run, in order.
pub async fn run_reporting(
    driver: &str,
    only: &[String],
    mut on_report: impl FnMut(&Report),
) -> Vec<Report> {
    let mut reports = Vec::new();
    for (name, scenario) in scenarios::SCENARIOS {
        if !only.is_empty() && !only.iter().any(|wanted| wanted == name) {
            continue;
        }
        let started = Instant::now();
        let outcome = tokio::time::timeout(SCENARIO_TIMEOUT, scenario(driver.to_string())).await;
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(why)) => Some(why),
            Err(_) => Some(format!("took longer than {SCENARIO_TIMEOUT:?}")),
        };
        let report = Report {
            name,
            took: started.elapsed(),
            failure,
        };
        on_report(&report);
        reports.push(report);
    }
    reports
}
