//! `popsub-scenarios`: runs the client scenario suite against a driver and
//! prints a line per scenario. Exits with a failure status if any fail.

use std::process::ExitCode;

use clap::Parser;

/// Runs the PopSub client scenario suite against a client library's driver.
#[derive(Parser)]
#[command(name = "popsub-scenarios")]
struct Args {
    /// Shell command that starts the driver, such as "node clients/typescript/dist/driver.js"
    #[arg(long, required_unless_present = "list")]
    driver: Option<String>,
    /// Run only this scenario; repeatable
    #[arg(long = "scenario")]
    scenarios: Vec<String>,
    /// Print the scenario names and exit
    #[arg(long)]
    list: bool,
}

/// Parses the arguments and runs or lists the scenarios.
///
/// Returns:
///     Success when every scenario run passed.
#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    if args.list {
        for (name, _) in popsub_scenarios::scenarios::SCENARIOS {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }
    let driver = args.driver.unwrap_or_default();
    let unknown: Vec<&String> = args
        .scenarios
        .iter()
        .filter(|wanted| {
            !popsub_scenarios::scenarios::SCENARIOS
                .iter()
                .any(|(name, _)| name == wanted)
        })
        .collect();
    if !unknown.is_empty() {
        eprintln!("unknown scenarios: {unknown:?}; --list shows them");
        return ExitCode::FAILURE;
    }
    let reports =
        popsub_scenarios::run_reporting(&driver, &args.scenarios, |report| match &report.failure {
            None => println!("PASS {} ({} ms)", report.name, report.took.as_millis()),
            Some(why) => println!(
                "FAIL {} ({} ms): {why}",
                report.name,
                report.took.as_millis()
            ),
        })
        .await;
    let failed = reports.iter().filter(|r| r.failure.is_some()).count();
    println!("{} passed, {failed} failed", reports.len() - failed);
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
