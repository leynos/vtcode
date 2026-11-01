//! Tests for patch-pipeline observability and precondition reporting.
//!
//! Covers what reaches the event stream: a preview render failure must log its
//! reason code without patch text, safety events must never carry patch
//! content, paths, or hashes, and a removed source file must report a null
//! current hash.

use super::log_patch_preview_render_failure;
use crate::tools::editing::patch::{PatchChunk, PatchLine, render_patch_update_content_strict};
use std::sync::{Arc, Mutex};

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

/// A preview-render failure must not leak patch text into tracing.
///
/// The subscriber is installed with `with_default` so parallel tests keep
/// their own dispatch and cannot contaminate the capture.
#[tokio::test]
async fn preview_render_failure_logs_reason_code_without_patch_text() {
    const MARKER: &str = "UNIQUE_PATCH_MARKER_c41f7d";
    let chunks = vec![PatchChunk {
        change_context: None,
        lines: vec![
            PatchLine::Removal(MARKER.to_string()),
            PatchLine::Addition("replacement".to_string()),
        ],
        is_end_of_file: false,
    }];
    let patch_error = render_patch_update_content_strict(
        std::path::Path::new("secret.txt"),
        "unrelated current line\n",
        &chunks,
        "secret.txt",
    )
    .await
    .expect_err("the marker anchor cannot match the current content");
    let error: anyhow::Error = patch_error.into();
    assert!(error.to_string().contains(MARKER), "precondition: the render error embeds patch text");

    let captured = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        log_patch_preview_render_failure(std::path::Path::new("secret.txt"), &error);
    });

    let output = String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8");
    assert!(output.contains("segment_not_found"), "reason code should be logged: {output}");
    assert!(!output.contains(MARKER), "patch text must not reach tracing: {output}");
}

/// Patch-safety diagnostics must stay bounded to enums and identifiers.
///
/// These records can be shipped to a log sink for a session editing
/// private files, so the file name, patch text, no-op signature, and
/// content hash must all be absent while the bounded outcomes are still
/// present.
#[tokio::test]
async fn patch_safety_events_never_carry_patch_text_paths_or_hashes() -> anyhow::Result<()> {
    const MARKER: &str = "UNIQUE_PATCH_MARKER_c41f7d";
    let temp_dir = tempfile::TempDir::new()?;
    let file_name = format!("{MARKER}.txt");
    let path = temp_dir.path().join(&file_name);
    std::fs::write(&path, format!("{MARKER}\n"))?;
    let on_disk_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(format!("{MARKER}\n").as_bytes()));

    let registry = crate::tools::ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;

    let no_op = format!("*** Begin Patch\n*** Update File: {file_name}\n@@\n-{MARKER}\n+{MARKER}\n*** End Patch\n");
    let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"a different revision\n"));
    let stale = format!("*** Begin Patch\n*** Update File: {file_name}\n@@\n-{MARKER}\n+changed\n*** End Patch\n");

    let captured = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let first = registry
        .execute_tool(crate::config::constants::tools::APPLY_PATCH, serde_json::json!({ "input": no_op }))
        .await?;
    assert_eq!(first["no_op"], true, "precondition: the first submission is a no-op: {first:?}");
    let signature = first["signature"].as_str().expect("no-op signature").to_string();

    let rejected = registry
        .execute_tool(
            crate::config::constants::tools::APPLY_PATCH,
            serde_json::json!({ "input": stale, "expected_content_hash": stale_hash }),
        )
        .await?;
    assert_eq!(
        rejected["error"]["details"]["reason"], "content_hash_mismatch",
        "precondition: the stale hash is rejected: {rejected:?}"
    );

    drop(guard);
    let output = String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8");
    assert!(output.contains("apply_patch no-op decision"), "the no-op decision must be logged: {output}");
    assert!(output.contains("content_hash_mismatch"), "the rejection outcome must be logged: {output}");
    assert!(!output.contains(MARKER), "patch text and file names must not reach tracing: {output}");
    assert!(!output.contains(&signature), "the no-op signature must not reach tracing: {output}");
    assert!(!output.contains(&stale_hash), "expected hashes must not reach tracing: {output}");
    assert!(!output.contains(&on_disk_hash), "on-disk hashes must not reach tracing: {output}");
    Ok(())
}

/// A source file removed after its version was captured must be reported as a
/// missing version, not as the malformed empty hash `sha256:`.
#[tokio::test]
async fn deleted_source_file_reports_a_null_current_hash() -> anyhow::Result<()> {
    let temp_dir = tempfile::TempDir::new()?;
    let path = temp_dir.path().join("gone.txt");
    std::fs::write(&path, "captured\n")?;
    let captured_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"captured\n"));
    std::fs::remove_file(&path)?;

    let registry = crate::tools::ToolRegistry::new(temp_dir.path().to_path_buf()).await;
    registry.allow_all_tools().await?;

    let patch = "*** Begin Patch\n*** Update File: gone.txt\n@@\n-captured\n+changed\n*** End Patch\n";
    let rejected = registry
        .execute_tool(
            crate::config::constants::tools::APPLY_PATCH,
            serde_json::json!({ "input": patch, "expected_content_hash": captured_hash }),
        )
        .await?;

    let details = &rejected["error"]["details"];
    assert_eq!(details["reason"], "content_hash_mismatch", "unexpected response: {rejected:?}");
    assert!(details["current_content_hash"].is_null(), "a removed file has no version: {rejected:?}");
    assert!(
        !rejected.to_string().contains("\"sha256:\""),
        "the malformed empty hash must not appear: {rejected:?}"
    );
    assert!(!path.exists(), "a rejected precondition must not recreate the file");
    Ok(())
}
