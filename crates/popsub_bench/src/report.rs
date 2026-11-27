//! Turns measurements into a text table for the terminal and a Markdown table for the docs.

use std::process::Command;

use crate::scenario::{GroupResult, RunOptions, ScenarioResult};

/// The column headings, shared by the text and Markdown tables.
const HEADINGS: [&str; 12] = [
    "scenario",
    "group",
    "load",
    "mode",
    "publishes/s",
    "deliveries/s",
    "p50 ms",
    "p99 ms",
    "max ms",
    "server cores",
    "server MiB",
    "loadgen cores",
];

/// How many of the leading columns hold text and are aligned left.
const TEXT_COLUMNS: usize = 4;

/// Formats a count with thousands separators.
///
/// Args:
///     value: The number, rounded to a whole count.
///
/// Returns:
///     Text such as `1,234,567`.
pub fn thousands(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Formats an optional reading to two decimal places.
///
/// Args:
///     value: The reading, if there is one.
///
/// Returns:
///     The number, or `-` when it is missing.
fn optional(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| format!("{v:.2}"))
}

/// Builds one table row for a group.
///
/// Args:
///     result: The scenario the group belongs to.
///     group: The group's measurements.
///
/// Returns:
///     The cells, in the order of `HEADINGS`.
fn row(result: &ScenarioResult, group: &GroupResult) -> Vec<String> {
    vec![
        result.name.to_string(),
        group.label.to_string(),
        group.load.clone(),
        group.mode.clone(),
        thousands(group.publishes_per_sec),
        thousands(group.deliveries_per_sec),
        optional(group.latency.map(|l| l.p50_ms)),
        optional(group.latency.map(|l| l.p99_ms)),
        optional(group.latency.map(|l| l.max_ms)),
        optional(result.server_cores),
        result
            .server_rss_mib
            .map_or_else(|| "-".to_string(), |mib| format!("{mib:.0}")),
        optional(result.loadgen_cores),
    ]
}

/// Lists what went wrong in a run, if anything.
///
/// Args:
///     results: Every scenario's measurements.
///
/// Returns:
///     One line per group that saw errors or didn't receive everything it was owed.
pub fn problems(results: &[ScenarioResult]) -> Vec<String> {
    let mut lines = Vec::new();
    for result in results {
        for group in &result.groups {
            if group.errors > 0 {
                lines.push(format!(
                    "{} ({}): {} error or unreadable frames",
                    result.name, group.label, group.errors
                ));
            }
            if !group.complete {
                lines.push(format!(
                    "{} ({}): not every publish reached every subscriber before the drain gave up",
                    result.name, group.label
                ));
            }
        }
    }
    lines
}

/// Collects every scenario's notes.
///
/// Args:
///     results: Every scenario's measurements.
///
/// Returns:
///     The notes, in the order the scenarios ran.
pub fn notes(results: &[ScenarioResult]) -> Vec<String> {
    results
        .iter()
        .flat_map(|result| result.notes.iter().cloned())
        .collect()
}

/// Renders the measurements as an aligned text table.
///
/// Args:
///     results: Every scenario's measurements.
///
/// Returns:
///     The table, one line per group, ending with a newline.
pub fn text_table(results: &[ScenarioResult]) -> String {
    let mut rows = vec![
        HEADINGS
            .iter()
            .map(|h| (*h).to_string())
            .collect::<Vec<_>>(),
    ];
    for result in results {
        for group in &result.groups {
            rows.push(row(result, group));
        }
    }
    let widths: Vec<usize> = (0..HEADINGS.len())
        .map(|column| rows.iter().map(|r| r[column].len()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for cells in &rows {
        let line: Vec<String> = cells
            .iter()
            .zip(&widths)
            .enumerate()
            .map(|(column, (cell, width))| {
                if column < TEXT_COLUMNS {
                    format!("{cell:<width$}")
                } else {
                    format!("{cell:>width$}")
                }
            })
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// Describes the machine the benchmark ran on.
///
/// Returns:
///     The CPU model when it can be read, the number of logical cores, and the operating system.
pub fn machine() -> String {
    let cores = std::thread::available_parallelism().map_or(0, usize::from);
    let model = cpu_model().unwrap_or_else(|| "unknown CPU".to_string());
    format!(
        "{model}, {cores} logical cores, {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Reads the CPU's model name.
///
/// Returns:
///     The name from `/proc/cpuinfo` on Linux or `sysctl` on macOS, or `None` elsewhere.
fn cpu_model() -> Option<String> {
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") {
        return info
            .lines()
            .find(|line| line.starts_with("model name"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, name)| name.trim().to_string());
    }
    let output = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Renders the measurements as a Markdown section, for pasting into `docs/BENCHMARKS.md`.
///
/// Args:
///     results: Every scenario's measurements.
///     options: The settings the run used.
///
/// Returns:
///     A settings list, the results table, and any problems.
pub fn markdown(results: &[ScenarioResult], options: &RunOptions) -> String {
    let mut out = String::new();
    out.push_str(&format!("- Broker: {}\n", options.target.label()));
    out.push_str(&format!("- Machine: {}\n", machine()));
    out.push_str(&format!(
        "- Storage backend: `{}`\n",
        options.backend.as_str()
    ));
    out.push_str(&format!(
        "- Window: {} s measured after {} s of warm-up\n",
        options.duration.as_secs(),
        options.warmup.as_secs()
    ));
    out.push_str(&format!("- Payload: {} bytes\n\n", options.payload_bytes));
    out.push_str(&format!("| {} |\n", HEADINGS.join(" | ")));
    out.push_str(&format!(
        "|{}|\n",
        HEADINGS
            .iter()
            .enumerate()
            .map(|(column, _)| if column < TEXT_COLUMNS { "---" } else { "---:" })
            .collect::<Vec<_>>()
            .join("|")
    ));
    for result in results {
        for group in &result.groups {
            out.push_str(&format!("| {} |\n", row(result, group).join(" | ")));
        }
    }
    let notes = notes(results);
    if !notes.is_empty() {
        out.push_str("\nNotes:\n\n");
        for line in notes {
            out.push_str(&format!("- {line}\n"));
        }
    }
    let problems = problems(results);
    if !problems.is_empty() {
        out.push_str("\nProblems:\n\n");
        for line in problems {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out
}
