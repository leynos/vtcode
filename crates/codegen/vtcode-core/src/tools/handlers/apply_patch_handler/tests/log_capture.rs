//! Decision-log capture for the `apply_patch` handler and direct runtime.
//!
//! Each test installs a scoped `tracing` subscriber with `set_default`, drives
//! the real entry point, and inspects the formatted records. The assertions
//! prove both halves of the contract: the bounded outcome and turn/call
//! identifiers are present, and no path, patch text, or hash reaches the log.
//! These live apart from the counter tests so each file stays within the
//! repository's 400-line limit.

use super::telemetry::{update_patch, versioned_fixture};
use super::*;
use serde_json::json;
use std::sync::Mutex;

/// A `tracing` writer that collects formatted output for one test only.
#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self
            .0
            .lock()
            .map_err(|poisoned| std::io::Error::other(format!("capture poisoned: {poisoned}")))?;
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Render a capture as UTF-8 for assertions.
fn captured_string(captured: &CaptureWriter) -> String {
    String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8")
}

/// Run `operation` under a scoped subscriber and return what it logged.
async fn capture_logs<F, Fut, T>(operation: F) -> (T, String)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let captured = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let output = operation().await;
    drop(guard);
    (output, captured_string(&captured))
}

/// A handler precondition rejection must leave a bounded decision log.
///
/// The handler runs outside the registry pipeline, so this record is the only
/// trace of why the patch was refused. It must name the fixed outcome and the
/// bounded identifiers without echoing the path, the patch, or the hash.
#[tokio::test]
async fn handler_precondition_rejection_logs_bounded_decision() {
    let workspace = TempDir::new().expect("temporary workspace");
    versioned_fixture(&workspace, "current\n");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));
    let arguments = json!({ "input": update_patch(), "expected_content_hash": stale_hash.clone() }).to_string();

    let (result, output) = capture_logs(|| async {
        ApplyPatchHandler
            .handle(test_invocation_with_metrics(workspace.path().to_path_buf(), arguments, None))
            .await
    })
    .await;

    assert!(result.is_err(), "a stale hash must be rejected");
    assert!(
        output.contains("apply_patch handler precondition decision"),
        "the handler decision must be logged: {output}"
    );
    assert!(output.contains("content_hash_mismatch"), "the fixed outcome must be logged: {output}");
    assert!(output.contains("apply-patch-handler-runtime-test"), "the turn identifier must be logged: {output}");
    assert!(output.contains("call-apply-patch"), "the call identifier must be logged: {output}");
    assert!(!output.contains("versioned.txt"), "paths must not reach tracing: {output}");
    assert!(!output.contains("*** Begin Patch"), "patch text must not reach tracing: {output}");
    assert!(!output.contains(&stale_hash), "expected hashes must not reach tracing: {output}");
}

/// A direct runtime rejection must leave a bounded revalidation log.
#[tokio::test]
async fn runtime_revalidation_rejection_logs_bounded_decision() {
    let workspace = TempDir::new().expect("workspace should be created");
    versioned_fixture(&workspace, "current\n");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));
    let tool_ctx = direct_runtime_ctx(&workspace);
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: update_patch().to_string(),
        expected_content_hash: Some(stale_hash.clone()),
        cwd: workspace.path().to_path_buf(),
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    let (result, output) =
        capture_logs(|| async { ApplyPatchRuntime::new().run(&request, &attempt, &tool_ctx).await }).await;

    assert!(result.is_err(), "a stale hash must fail the runtime revalidation");
    assert!(
        output.contains("apply_patch runtime revalidation decision"),
        "the runtime decision must be logged: {output}"
    );
    assert!(output.contains("content_hash_mismatch"), "the fixed outcome must be logged: {output}");
    assert!(output.contains("direct-apply-patch-test"), "the turn identifier must be logged: {output}");
    assert!(!output.contains("versioned.txt"), "paths must not reach tracing: {output}");
    assert!(!output.contains(&stale_hash), "expected hashes must not reach tracing: {output}");
}

/// A matching direct-runtime revalidation must log the match, not only failures.
#[tokio::test]
async fn runtime_revalidation_match_logs_bounded_decision() {
    let workspace = TempDir::new().expect("workspace should be created");
    let current_hash = versioned_fixture(&workspace, "current\n");
    let tool_ctx = direct_runtime_ctx(&workspace);
    let policy = SandboxConfig::default();
    let attempt = sandbox_attempt(&policy, &workspace);
    let request = ApplyPatchRequest {
        patch: update_patch().to_string(),
        expected_content_hash: Some(current_hash.clone()),
        cwd: workspace.path().to_path_buf(),
        timeout_ms: None,
        user_explicitly_approved: true,
    };

    let (result, output) =
        capture_logs(|| async { ApplyPatchRuntime::new().run(&request, &attempt, &tool_ctx).await }).await;

    result.expect("a matching hash must apply through the direct runtime");
    assert!(
        output.contains("apply_patch runtime revalidation decision"),
        "the runtime decision must be logged: {output}"
    );
    assert!(output.contains("matched"), "the matched outcome must be logged: {output}");
    assert!(output.contains("direct-apply-patch-test"), "the turn identifier must be logged: {output}");
    assert!(!output.contains(&current_hash), "expected hashes must not reach tracing: {output}");
}
