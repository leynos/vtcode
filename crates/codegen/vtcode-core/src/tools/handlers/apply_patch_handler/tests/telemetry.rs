//! Telemetry for the `ApplyPatchHandler` and direct-runtime entry points.
//!
//! These drive the real entry points with a session-attached collector and
//! inspect the exported snapshot, so the counters prove what the handler did
//! rather than what a helper was asked to do. Every assertion also checks that
//! no patch text or content hash reached the export.

use super::*;
use crate::metrics::{MetricsCollector, PatchSafetyMetrics};
use serde_json::json;

/// Write a UTF-8 fixture and return its canonical version.
pub(super) fn versioned_fixture(workspace: &TempDir, content: &str) -> String {
    std::fs::write(workspace.path().join("versioned.txt"), content).expect("write fixture");
    format!("sha256:{}", vtcode_commons::utils::calculate_sha256(content.as_bytes()))
}

pub(super) fn update_patch() -> &'static str {
    "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n"
}

/// Assert both exports carry counters only, never content or failure detail.
///
/// Paths, patch text, hashes, signatures, and error strings are all
/// caller-supplied, so neither export may echo any of them.
fn assert_export_is_content_free(metrics: &MetricsCollector) {
    let json = metrics.export_json().expect("metrics JSON should serialize").to_string();
    let prometheus = metrics.export_prometheus();
    for (format, rendered) in [("JSON", &json), ("Prometheus", &prometheus)] {
        for sentinel in [
            "versioned.txt",
            "*** Begin Patch",
            "sha256:",
            "signature",
            "Unable to version",
        ] {
            assert!(!rendered.contains(sentinel), "{sentinel:?} leaked into the {format} export: {rendered}");
        }
    }
}

fn patch_safety(metrics: &MetricsCollector) -> PatchSafetyMetrics {
    metrics.get_summary().patch_safety
}

/// A matching request counts one preflight match, one runtime revalidation,
/// and exactly one evaluation: the handler owns the evaluation, so the runtime
/// it drives must not count a second.
#[tokio::test]
async fn handler_counts_preflight_revalidation_and_one_evaluation() {
    let workspace = TempDir::new().expect("temporary workspace");
    let current_hash = versioned_fixture(&workspace, "current\n");
    let metrics = Arc::new(MetricsCollector::new());
    let arguments = json!({ "input": update_patch(), "expected_content_hash": current_hash }).to_string();

    ApplyPatchHandler
        .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, Some(Arc::clone(&metrics))))
        .await
        .expect("a matching hash must apply");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.precondition_matched, 1);
    assert_eq!(safety.runtime_revalidation_matched, 1);
    assert_eq!(safety.runtime_revalidation_mismatch, 0);
    assert_eq!(safety.evaluations, 1, "one logical request must be counted once");
    assert_export_is_content_free(&metrics);

    let prometheus = metrics.export_prometheus();
    assert!(prometheus.contains(r#"vtcode_apply_patch_precondition_total{outcome="matched"} 1"#));
    assert!(prometheus.contains(r#"vtcode_apply_patch_runtime_revalidation_total{outcome="matched"} 1"#));
    assert!(prometheus.contains("vtcode_apply_patch_evaluations_total 1"));
}

/// A stale hash is rejected by the preflight, so only the preflight counter
/// moves and the evaluation is still counted.
#[tokio::test]
async fn stale_hash_counts_one_mismatch_and_no_runtime_revalidation() {
    let workspace = TempDir::new().expect("temporary workspace");
    versioned_fixture(&workspace, "current\n");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));
    let metrics = Arc::new(MetricsCollector::new());
    let arguments = json!({ "input": update_patch(), "expected_content_hash": stale_hash }).to_string();

    ApplyPatchHandler
        .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, Some(Arc::clone(&metrics))))
        .await
        .expect_err("a stale hash must be rejected");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.precondition_content_hash_mismatch, 1);
    assert_eq!(safety.runtime_revalidation_matched, 0);
    assert_eq!(safety.runtime_revalidation_mismatch, 0);
    assert_eq!(safety.evaluations, 1);
    assert_export_is_content_free(&metrics);
    assert!(
        metrics
            .export_prometheus()
            .contains(r#"vtcode_apply_patch_precondition_total{outcome="content_hash_mismatch"} 1"#)
    );
}

/// A removed source file is its own outcome, distinct from a content change.
#[tokio::test]
async fn missing_source_file_counts_the_missing_file_outcome() {
    let workspace = TempDir::new().expect("temporary workspace");
    let expected_hash = versioned_fixture(&workspace, "captured\n");
    std::fs::remove_file(workspace.path().join("versioned.txt")).expect("remove fixture");
    let metrics = Arc::new(MetricsCollector::new());
    let arguments = json!({ "input": update_patch(), "expected_content_hash": expected_hash }).to_string();

    ApplyPatchHandler
        .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, Some(Arc::clone(&metrics))))
        .await
        .expect_err("a removed source file must be rejected");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.precondition_missing_file, 1);
    assert_eq!(safety.precondition_content_hash_mismatch, 0);
    assert_eq!(safety.evaluations, 1);
    assert_export_is_content_free(&metrics);
    assert!(
        metrics
            .export_prometheus()
            .contains(r#"vtcode_apply_patch_precondition_total{outcome="missing_file"} 1"#)
    );
}

/// An out-of-scope hash is counted separately from a version mismatch.
#[tokio::test]
async fn invalid_scope_counts_the_invalid_scope_outcome() {
    let workspace = TempDir::new().expect("temporary workspace");
    let metrics = Arc::new(MetricsCollector::new());
    let arguments = json!({
        "input": "*** Begin Patch\n*** Add File: fresh.txt\n+hello\n*** End Patch\n",
        "expected_content_hash": format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"anything\n")),
    })
    .to_string();

    ApplyPatchHandler
        .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, Some(Arc::clone(&metrics))))
        .await
        .expect_err("an out-of-scope hash must be rejected");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.precondition_invalid_scope, 1);
    assert_eq!(safety.evaluations, 1);
    assert_export_is_content_free(&metrics);
    assert!(
        metrics
            .export_prometheus()
            .contains(r#"vtcode_apply_patch_precondition_total{outcome="invalid_scope"} 1"#)
    );
}

/// A malformed hash is rejected during decoding and still counts the request.
#[tokio::test]
async fn malformed_hash_counts_the_malformed_outcome_and_the_evaluation() {
    let workspace = TempDir::new().expect("temporary workspace");
    versioned_fixture(&workspace, "current\n");
    let metrics = Arc::new(MetricsCollector::new());
    let arguments = json!({ "input": update_patch(), "expected_content_hash": "not-a-hash" }).to_string();

    ApplyPatchHandler
        .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, Some(Arc::clone(&metrics))))
        .await
        .expect_err("a malformed hash must be rejected");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.precondition_malformed_hash, 1);
    assert_eq!(safety.evaluations, 1, "early rejection must still count the evaluation");
    assert_export_is_content_free(&metrics);
    assert!(
        metrics
            .export_prometheus()
            .contains(r#"vtcode_apply_patch_precondition_total{outcome="malformed_hash"} 1"#)
    );
}

/// A source path that cannot be read is a validation failure, not a version
/// mismatch. A directory standing in for the source produces a read error on
/// every supported platform.
#[tokio::test]
async fn unreadable_source_counts_the_validation_failed_outcome() {
    let workspace = TempDir::new().expect("temporary workspace");
    std::fs::create_dir(workspace.path().join("versioned.txt")).expect("create directory fixture");
    let metrics = Arc::new(MetricsCollector::new());
    let arguments = json!({
        "input": update_patch(),
        "expected_content_hash": format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"anything\n")),
    })
    .to_string();

    ApplyPatchHandler
        .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, Some(Arc::clone(&metrics))))
        .await
        .expect_err("an unreadable source must be rejected");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.precondition_validation_failed, 1);
    assert_eq!(safety.evaluations, 1);
    assert_export_is_content_free(&metrics);
    assert!(
        metrics
            .export_prometheus()
            .contains(r#"vtcode_apply_patch_precondition_total{outcome="validation_failed"} 1"#)
    );
}

/// A directly driven runtime owns its own evaluation and revalidation, so it
/// keeps telemetry when there is no handler above it.
#[tokio::test]
async fn direct_runtime_records_its_own_evaluation_and_revalidation() {
    let workspace = TempDir::new().expect("workspace should be created");
    let current_hash = versioned_fixture(&workspace, "current\n");
    let metrics = Arc::new(MetricsCollector::new());
    let tool_ctx = direct_runtime_ctx_with_metrics(&workspace, Some(Arc::clone(&metrics)));
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: update_patch().to_string(),
        expected_content_hash: Some(current_hash),
        cwd: workspace.path().to_path_buf(),
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    ApplyPatchRuntime::new()
        .run(&request, &attempt, &tool_ctx)
        .await
        .expect("a matching hash must apply through the direct runtime");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.evaluations, 1, "the direct runtime must count its own evaluation");
    assert_eq!(safety.runtime_revalidation_matched, 1);
    assert_eq!(safety.precondition_matched, 0, "a direct runtime has no handler preflight to count");
    assert_export_is_content_free(&metrics);
    assert!(metrics.export_prometheus().contains("vtcode_apply_patch_evaluations_total 1"));
}

/// A stale hash fails the direct runtime's own revalidation. The failure is
/// counted, the evaluation is still recorded exactly once, and neither export
/// picks up the rejected hash.
#[tokio::test]
async fn direct_runtime_counts_a_failed_revalidation() {
    let workspace = TempDir::new().expect("workspace should be created");
    versioned_fixture(&workspace, "current\n");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));
    let metrics = Arc::new(MetricsCollector::new());
    let tool_ctx = direct_runtime_ctx_with_metrics(&workspace, Some(Arc::clone(&metrics)));
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: update_patch().to_string(),
        expected_content_hash: Some(stale_hash),
        cwd: workspace.path().to_path_buf(),
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    ApplyPatchRuntime::new()
        .run(&request, &attempt, &tool_ctx)
        .await
        .expect_err("a stale hash must fail the direct runtime's revalidation");

    let safety = patch_safety(&metrics);
    assert_eq!(safety.runtime_revalidation_mismatch, 1);
    assert_eq!(safety.runtime_revalidation_matched, 0);
    assert_eq!(safety.evaluations, 1, "a failed revalidation still counts one evaluation");
    assert_eq!(safety.precondition_content_hash_mismatch, 0, "a direct runtime has no preflight");
    assert_export_is_content_free(&metrics);
    assert!(
        metrics
            .export_prometheus()
            .contains(r#"vtcode_apply_patch_runtime_revalidation_total{outcome="mismatch"} 1"#)
    );
}
