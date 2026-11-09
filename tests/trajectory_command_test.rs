#![allow(
    missing_docs,
    clippy::expect_used,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use assert_cmd::Command;
use std::path::Path;

/// A representative trajectory log covering routes, tools, prompt-cache
/// metrics, cache-churn reasons, and two tool-catalogue order snapshots.
///
/// The record kinds and change reasons keep their historical `catalog`
/// spelling; only the rendered report labels use `catalogue`.
const TRAJECTORY_LOG: &str = r#"
{"kind":"route","turn":1,"selected_model":"gpt-5","class":"default","ts":1}
{"kind":"tool","turn":1,"name":"exec_command","args":{},"ok":true,"ts":2}
{"kind":"tool","turn":2,"name":"exec_command","args":{},"ok":false,"ts":3}
{"kind":"prompt_cache_metrics","turn":1,"model":"gpt-5","prompt_tokens":1000,"cached_prompt_tokens":400,"ts":4}
{"kind":"tool_catalog_cache_metrics","turn":1,"model":"gpt-5","prefix_change_reason":"model","ordered_wire_tool_names":["exec_command","search_tools"],"catalog_tool_count":5,"wire_tool_count":2,"deferred_tool_count":3,"ts":5}
{"kind":"tool_catalog_cache_metrics","turn":2,"model":"gpt-5","prefix_change_reason":"unchanged","ts":6}
{"kind":"tool_catalog_cache_metrics","turn":3,"model":"gpt-5","prefix_change_reason":"tool_catalog","ordered_wire_tool_names":["search_tools","exec_command"],"catalog_tool_count":5,"wire_tool_count":2,"deferred_tool_count":3,"ts":7}
"#;

/// Build a `diapason` command whose configuration and state stay inside an
/// isolated home directory.
fn isolated_diapason_command(home: &Path) -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("diapason");
    let _configured_command = cmd
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".state"))
        .env("XDG_DATA_HOME", home.join(".data"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env_remove("DIAPASON_CONFIG_PATH")
        .current_dir(home)
        .env("NO_COLOR", "1")
        // The report reads a log file and never contacts a provider, but
        // startup still requires the default provider's key to be present.
        .env("OPENROUTER_API_KEY", "trajectory-report-test-key");
    cmd
}

/// Remove ANSI colour escape sequences so assertions can match the rendered
/// labels and counts as plain text.
fn strip_ansi(input: &str) -> String {
    let mut plain = String::with_capacity(input.len());
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        if characters.next() == Some('[') {
            // Consume the CSI parameter and intermediate bytes; the sequence
            // ends with a final byte in the range `@` to `~`.
            for final_byte in characters.by_ref() {
                if ('@'..='~').contains(&final_byte) {
                    break;
                }
            }
        }
    }
    plain
}

#[test]
fn trajectory_command_prints_catalogue_labels_and_counts() {
    let home = tempfile::tempdir().expect("create temp home");
    let log = home.path().join("trajectory.jsonl");
    std::fs::write(&log, TRAJECTORY_LOG).expect("write trajectory log");

    let mut cmd = isolated_diapason_command(home.path());
    let output = cmd
        .arg("trajectory")
        .arg("--file")
        .arg(&log)
        .output()
        .expect("run diapason trajectory");

    assert!(output.status.success(), "trajectory report failed: {}", String::from_utf8_lossy(&output.stderr));

    let report = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    let expected = [
        "Trajectory Report",
        "1 routes, 2 tools, 1 cache metrics",
        "Cache Churn",
        "tool_catalogue: 1",
        "Tool Catalogue Order",
        "snapshots: 2",
        "order_changes: 1",
        "catalogue: 5",
        "wire: 2",
        "deferred: 3",
    ];
    for label in expected {
        assert!(report.contains(label), "missing `{label}` in report:\n{report}");
    }
    assert!(!report.contains("Tool Catalog Order"), "report kept the old section title:\n{report}");
}
