//! Certification outcomes for the ACP versioned read.
//!
//! Each failure test drives a real `read_file` through the official ACP
//! transport and asserts three things: the exact certification counter moves
//! once, the bounded warning carries only the fixed `operation`/`outcome`
//! fields, and neither metrics export leaks the path, a hash, patch text, or
//! error text. The certified case asserts the opposite: no counter moves.

use super::read_file_version::{
    ClientWindowMode, FixtureMutation, WindowedReadFlow, run_windowed_read_flow, run_windowed_read_flow_with_bytes,
    tool_result_payload,
};
use rstest::rstest;

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
    counted: impl Fn(&diapason_core::metrics::PatchSafetyMetrics) -> u64,
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

/// A windowed read of unchanged bytes certifies the whole-file version.
#[tokio::test]
async fn windowed_read_certifies_the_whole_file_and_patches_with_its_hash() {
    let disk = "alpha\nbeta\ngamma\n";
    let whole_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(disk.as_bytes()));
    let read = serde_json::json!({"path": "windowed.txt", "line": 2, "limit": 1}).to_string();
    let patch = "*** Begin Patch\n*** Update File: windowed.txt\n@@\n-beta\n+beta2\n*** End Patch\n";
    let tool_calls = vec![
        ("read_file".to_string(), read),
        (
            "apply_patch".to_string(),
            serde_json::json!({"input": patch, "expected_content_hash": whole_hash.clone()}).to_string(),
        ),
    ];

    let flow = run_windowed_read_flow("windowed.txt", disk, None, tool_calls, None, ClientWindowMode::Honours).await;

    assert_eq!(flow.calls, 3, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    assert_eq!(tool_result_payload(&requests, 1)["content_hash"], whole_hash);
    assert_eq!(tool_result_payload(&requests, 2)["result"]["success"], true);
    assert_eq!(std::fs::read_to_string(&flow.path).expect("read final fixture"), "alpha\nbeta2\ngamma\n");

    // A certified read is not a failure outcome, so no certification counter
    // may move: the counters exist to explain withheld versions.
    let certification = flow.certification();
    assert_eq!(certification.certification_window_projection_mismatch, 0);
    assert_eq!(certification.certification_window_read_failed, 0);
    assert_eq!(certification.certification_window_not_utf8, 0);
    assert_eq!(certification.certification_read_content_mismatch, 0);
    assert_eq!(certification.certification_read_version_capture_failed, 0);
}

/// A dirty buffer window never certifies the on-disk file, and its stale hash
/// is rejected before any write.
#[tokio::test]
async fn windowed_read_of_a_dirty_buffer_returns_no_version_and_rejects_the_stale_hash() {
    let disk = "alpha\nbeta\ngamma\n";
    let buffer = "alpha\nBETA\ngamma\n";
    let buffer_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(buffer.as_bytes()));
    let read = serde_json::json!({"path": "buffer.txt", "line": 2, "limit": 1}).to_string();
    let patch = "*** Begin Patch\n*** Update File: buffer.txt\n@@\n-beta\n+beta2\n*** End Patch\n";
    let tool_calls = vec![
        ("read_file".to_string(), read),
        (
            "apply_patch".to_string(),
            serde_json::json!({"input": patch, "expected_content_hash": buffer_hash}).to_string(),
        ),
    ];

    let flow =
        run_windowed_read_flow("buffer.txt", disk, Some(buffer), tool_calls, None, ClientWindowMode::Honours).await;

    assert_eq!(flow.calls, 3, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    assert!(
        tool_result_payload(&requests, 1)["content_hash"].is_null(),
        "a dirty buffer window must not certify the on-disk bytes"
    );
    assert_eq!(tool_result_payload(&requests, 2)["error"]["details"]["reason"], "content_hash_mismatch");
    assert_eq!(std::fs::read_to_string(&flow.path).expect("read final fixture"), disk);

    let certification = flow.certification();
    assert_eq!(
        certification.certification_window_projection_mismatch, 1,
        "a dirty buffer window must be counted as a projection mismatch"
    );
    assert_eq!(certification.certification_window_not_utf8, 0);
}

/// A client that returns a different amount of text than the requested window
/// never received the bytes a version would describe, so no version is issued.
///
/// The ignored case makes the client return the whole file for a one-line
/// window; the disk ends with a blank line, so collapsing every trailing
/// newline would make the two sides compare equal and certify a response that
/// carries content the window never included. The truncated case returns only
/// the first line of a two-line window; that response is itself
/// newline-terminated, so it is a complete set of lines that is short by one
/// and must not be mistaken for the projection minus a final terminator.
#[rstest]
#[case::ignored_window(
    "ignored.txt",
    "alpha\n\n",
    1,
    1,
    ClientWindowMode::Ignores,
    "a response wider than the requested window must not certify the on-disk bytes"
)]
#[case::truncated_window(
    "truncated.txt",
    "alpha\nbeta\n",
    1,
    2,
    ClientWindowMode::Truncates,
    "a truncated response must not certify the on-disk window"
)]
#[tokio::test]
async fn read_window_outside_the_requested_slice_returns_no_version(
    #[case] file_name: &str,
    #[case] disk: &str,
    #[case] line: u32,
    #[case] limit: u32,
    #[case] window_mode: ClientWindowMode,
    #[case] rejection_message: &str,
) {
    let read = serde_json::json!({"path": file_name, "line": line, "limit": limit}).to_string();
    let tool_calls = vec![("read_file".to_string(), read)];

    let flow = run_windowed_read_flow(file_name, disk, None, tool_calls, None, window_mode).await;

    assert_eq!(flow.calls, 2, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    let payload = tool_result_payload(&requests, 1);
    assert!(payload["content_hash"].is_null(), "{rejection_message}");
    assert_eq!(std::fs::read_to_string(&flow.path).expect("read final fixture"), disk);

    let certification = flow.certification();
    assert_eq!(
        certification.certification_window_projection_mismatch, 1,
        "a response outside the requested slice must be counted as a projection mismatch: {rejection_message}"
    );
    assert_eq!(certification.certification_window_not_utf8, 0);
    assert_eq!(certification.certification_window_read_failed, 0);
}

/// The on-disk bytes being non-UTF-8 gets its own counter, distinct from a
/// projection mismatch, so an operator can tell a binary file from an editor
/// returning the wrong window.
#[tokio::test]
async fn windowed_read_of_a_non_utf8_file_counts_the_not_utf8_outcome() {
    let read = serde_json::json!({"path": "binary.bin", "line": 1, "limit": 1}).to_string();
    let tool_calls = vec![("read_file".to_string(), read)];

    // The client answers with valid UTF-8 while the file on disk is not, which
    // is exactly the case the on-disk UTF-8 check exists for.
    let flow = run_windowed_read_flow_with_bytes(
        "binary.bin",
        "",
        Some(&[0xff, 0xfe, b'\n']),
        Some("alpha\n"),
        tool_calls,
        None,
        ClientWindowMode::Honours,
    )
    .await;

    assert_eq!(flow.calls, 2, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    let payload = tool_result_payload(&requests, 1);
    assert!(payload["content_hash"].is_null(), "a non-UTF-8 file must not be certified");

    let certification = flow.certification();
    assert_eq!(
        certification.certification_window_not_utf8, 1,
        "non-UTF-8 on-disk content must be counted separately from a mismatch"
    );
    assert_eq!(certification.certification_window_projection_mismatch, 0);
    assert_eq!(certification.certification_window_read_failed, 0);
}
