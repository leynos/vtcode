//! Certification failure paths for the ACP versioned read.
//!
//! Each test drives a real `read_file` through the official ACP transport and
//! asserts three things: the exact certification counter moves once, the
//! bounded warning carries only the fixed `operation`/`outcome` fields, and
//! neither metrics export leaks the path, a hash, patch text, or error text.

use super::read_file_version::{
    ClientWindowMode, FixtureMutation, WindowedReadFlow, run_windowed_read_flow, tool_result_payload,
};

/// Assert the captured certification warnings are bounded and content-free.
///
/// Only events tagged `operation="read_file"` are certification warnings; the
/// rest of the ACP log is unrelated to this assertion.
fn assert_bounded_certification_warning(flow: &WindowedReadFlow, expected_outcome: &str, file_name: &str) {
    let certification: Vec<&String> = flow
        .events
        .iter()
        .filter(|event| event.contains("operation=\"read_file\""))
        .collect();
    assert_eq!(certification.len(), 1, "exactly one certification warning expected, got {:?}", flow.events);
    let event = certification[0];
    assert!(
        event.contains(&format!("outcome=\"{expected_outcome}\"")),
        "expected outcome {expected_outcome:?} in {event}"
    );
    // Paths, content hashes, patch text, and error strings are caller-supplied
    // and must never reach the diagnostics pipeline.
    for sentinel in [
        file_name,
        "sha256:",
        "*** Begin Patch",
        "signature",
        "No such file",
        "Unable to version",
    ] {
        assert!(!event.contains(sentinel), "sensitive sentinel {sentinel:?} leaked into the log: {event}");
    }
}

/// Assert neither metrics export carries content or failure detail.
fn assert_exports_are_content_free(flow: &WindowedReadFlow, file_name: &str) {
    let json = flow.metrics.export_json().expect("metrics JSON should serialize").to_string();
    let prometheus = flow.metrics.export_prometheus();
    for (format, rendered) in [("JSON", &json), ("Prometheus", &prometheus)] {
        for sentinel in [
            file_name,
            "sha256:",
            "*** Begin Patch",
            "signature",
            "Unable to version",
        ] {
            assert!(!rendered.contains(sentinel), "{sentinel:?} leaked into the {format} export: {rendered}");
        }
    }
}

/// Exactly one certification outcome is counted, and it is the one under test.
///
/// Summing every category proves the failure path neither double-counts nor
/// spills into a neighbouring outcome.
fn assert_only_certification_outcome_is(
    flow: &WindowedReadFlow,
    counted: impl Fn(&vtcode_core::metrics::PatchSafetyMetrics) -> u64,
    message: &str,
) {
    let certification = flow.certification();
    let total = certification.certification_window_projection_mismatch
        + certification.certification_window_read_failed
        + certification.certification_window_not_utf8
        + certification.certification_read_content_mismatch
        + certification.certification_read_version_capture_failed;
    assert_eq!(total, 1, "exactly one certification outcome must be counted: {certification:?}");
    assert_eq!(counted(&certification), 1, "{message}");
}

/// A windowed read whose on-disk file disappeared is a read failure, not a
/// projection mismatch: the client returned a plausible window, but the bytes
/// to certify are gone.
#[tokio::test]
async fn windowed_read_of_a_removed_file_counts_the_read_failed_outcome() {
    let read = serde_json::json!({"path": "removed.txt", "line": 1, "limit": 1}).to_string();
    let tool_calls = vec![("read_file".to_string(), read)];

    // The client serves an unsaved buffer while the file is removed before the
    // call, so only the on-disk certification read fails.
    let flow = run_windowed_read_flow(
        "removed.txt",
        "alpha\n",
        Some("alpha\n"),
        tool_calls,
        Some(FixtureMutation::Remove { index: 0 }),
        ClientWindowMode::Honours,
    )
    .await;

    assert_eq!(flow.calls, 2, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    assert!(
        tool_result_payload(&requests, 1)["content_hash"].is_null(),
        "a read failure must withhold the version"
    );

    assert_only_certification_outcome_is(
        &flow,
        |metrics| metrics.certification_window_read_failed,
        "a removed file must be counted as a window read failure",
    );
    assert_bounded_certification_warning(&flow, "window_read_failed", "removed.txt");
    assert_exports_are_content_free(&flow, "removed.txt");
}

/// A whole-file read of a dirty buffer never certifies the on-disk bytes.
#[tokio::test]
async fn whole_file_read_of_a_dirty_buffer_counts_the_content_mismatch_outcome() {
    let read = serde_json::json!({"path": "dirty.txt"}).to_string();
    let tool_calls = vec![("read_file".to_string(), read)];

    let flow =
        run_windowed_read_flow("dirty.txt", "alpha\n", Some("dirty\n"), tool_calls, None, ClientWindowMode::Honours)
            .await;

    assert_eq!(flow.calls, 2, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    assert!(
        tool_result_payload(&requests, 1)["content_hash"].is_null(),
        "a dirty buffer must not certify the on-disk bytes"
    );

    assert_only_certification_outcome_is(
        &flow,
        |metrics| metrics.certification_read_content_mismatch,
        "a dirty buffer must be counted as a content mismatch",
    );
    assert_bounded_certification_warning(&flow, "read_content_mismatch", "dirty.txt");
    assert_exports_are_content_free(&flow, "dirty.txt");
}

/// A whole-file read whose version cannot be captured fails closed and is
/// counted separately from a content mismatch.
#[tokio::test]
async fn whole_file_read_of_a_removed_file_counts_the_capture_failed_outcome() {
    let read = serde_json::json!({"path": "gone.txt"}).to_string();
    let tool_calls = vec![("read_file".to_string(), read)];

    let flow = run_windowed_read_flow(
        "gone.txt",
        "alpha\n",
        Some("alpha\n"),
        tool_calls,
        Some(FixtureMutation::Remove { index: 0 }),
        ClientWindowMode::Honours,
    )
    .await;

    assert_eq!(flow.calls, 2, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    assert!(
        tool_result_payload(&requests, 1)["content_hash"].is_null(),
        "a failed capture must withhold the version"
    );

    assert_only_certification_outcome_is(
        &flow,
        |metrics| metrics.certification_read_version_capture_failed,
        "a missing snapshot must be counted as a capture failure",
    );
    assert_bounded_certification_warning(&flow, "read_version_capture_failed", "gone.txt");
    assert_exports_are_content_free(&flow, "gone.txt");
}
