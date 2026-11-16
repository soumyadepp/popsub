//! Runs the whole scenario suite against the Rust client's driver, so the
//! suite and the reference client are checked together by `make test`.

/// Checks that the Rust client passes every scenario.
#[tokio::test(flavor = "multi_thread")]
async fn the_rust_client_passes_every_scenario() {
    let reports = popsub_scenarios::run(env!("CARGO_BIN_EXE_popsub-sdk-driver"), &[]).await;
    let failures: Vec<String> = reports
        .iter()
        .filter_map(|r| r.failure.as_ref().map(|why| format!("{}: {why}", r.name)))
        .collect();
    assert!(!reports.is_empty());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
