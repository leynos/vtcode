//! `apply_patch` planning, precondition checking, and mutation ordering.
//!
//! The applicator replays a patch's operations in order against live file state.
//! This module mirrors that order: it canonicalises the touched paths, acquires
//! mutation leases, snapshots each path, and then plans every write from the
//! preceding operation's *result* rather than from the pre-patch snapshot. That
//! carry-forward is what keeps the no-op guard honest when a patch updates the
//! same path more than once, since planning from the original snapshot would
//! make a productive chain look like a no-op.
//!
//! Version and anchor preconditions are validated in the sibling
//! `patch_preconditions` module; the decision state machine itself lives in
//! `crate::tools::registry::patch_guard`.

use super::ToolRegistry;
use super::patch_preconditions::{invalid_patch_precondition, repeated_no_op_error, validate_patch_content_hash};
use crate::config::constants::tools;
use crate::metrics::{PatchNoOpOutcome, PatchPreconditionOutcome};
use crate::tools::edited_file_monitor::{FileSnapshot, MutationLease, conflict_override_snapshot};
use crate::tools::registry::patch_guard::{CanonicalPatchOperation, NoOpPatchDecision, PatchPathState};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

/// Records how long one `apply_patch` evaluation took when it returns.
///
/// A guard rather than a call at each exit keeps the timing honest: rejections,
/// planning failures, and successful writes all return through a different
/// branch, and every one of them is an evaluation worth counting.
struct PatchEvaluationTimer<'a> {
    registry: &'a ToolRegistry,
    started: std::time::Instant,
}

impl Drop for PatchEvaluationTimer<'_> {
    /// Record the finished `apply_patch` evaluation duration.
    fn drop(&mut self) {
        let duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.registry.metrics_collector().record_patch_evaluation(duration_ms);
    }
}

enum PlannedPatchWrite {
    Text {
        path: PathBuf,
        before: Option<String>,
        content: String,
    },
    Removal {
        path: PathBuf,
        before: Option<String>,
    },
}

pub(super) struct CapturedPatchPath {
    pub(super) path: PathBuf,
    pub(super) snapshot: FileSnapshot,
}

impl ToolRegistry {
    /// Plan, validate, and apply one patch through the version and no-op guards.
    ///
    /// Every mutation path is snapshotted before planning, so the version
    /// precondition and the no-op ladder compare the same bytes the applicator
    /// will write.
    pub(super) async fn execute_apply_patch_internal(&self, args: Value) -> Result<Value> {
        let _evaluation_timer = PatchEvaluationTimer { registry: self, started: std::time::Instant::now() };
        let context = self.harness_context_snapshot();
        let patch_input = crate::tools::apply_patch::decode_apply_patch_input(&args)?
            .ok_or_else(|| anyhow!("Missing patch input {}", crate::tools::error_helpers::PATCH_PARAMETER_HINT))?;
        let override_snapshot = conflict_override_snapshot(&args);

        let patch = crate::tools::editing::Patch::parse(&patch_input.text)?;
        let mutation_paths = self.patch_mutation_paths(&patch).await?;
        let _mutation_leases = self.acquire_patch_mutations(&mutation_paths).await;
        let captured_paths = self.capture_patch_paths(&mutation_paths).await?;
        let expected_hash = match crate::tools::apply_patch::expected_content_hash_from_args(&args) {
            Ok(value) => value.map(ToOwned::to_owned),
            Err(error) => {
                self.metrics_collector()
                    .record_patch_precondition(PatchPreconditionOutcome::MalformedHash);
                tracing::info!(
                    operation = tools::APPLY_PATCH,
                    outcome = PatchPreconditionOutcome::MalformedHash.as_str(),
                    session_id = %context.session_id,
                    task_id = %context.task_id.as_deref().unwrap_or(""),
                    "apply_patch version precondition rejected"
                );
                return Ok(invalid_patch_precondition(
                    error.to_string(),
                    json!({
                        "reason": "invalid_expected_content_hash",
                        "next_action": "Reread the file and use the exact content_hash returned by read_file."
                    }),
                ));
            }
        };
        if let Some(expected_hash) = expected_hash {
            match validate_patch_content_hash(self, &patch, &expected_hash, &captured_paths).await? {
                Some(rejection) => {
                    self.metrics_collector().record_patch_precondition(rejection.outcome);
                    tracing::info!(
                        operation = tools::APPLY_PATCH,
                        outcome = rejection.outcome.as_str(),
                        session_id = %context.session_id,
                        task_id = %context.task_id.as_deref().unwrap_or(""),
                        "apply_patch version precondition rejected"
                    );
                    return Ok(rejection.response);
                }
                None => self
                    .metrics_collector()
                    .record_patch_precondition(PatchPreconditionOutcome::Matched),
            }
        }
        for operation in patch.operations() {
            if let Some(conflict) = self
                .detect_patch_operation_conflict(operation, override_snapshot.clone())
                .await?
            {
                return Ok(conflict.to_tool_output(self.workspace_root()));
            }
        }
        let planned_writes = self.planned_patch_writes(&patch, &captured_paths).await?;
        let (before, after) = self.planned_patch_states(&captured_paths, &planned_writes);
        let canonical_operations = self.canonical_patch_operations(&patch).await?;
        match self
            .no_op_patch_guard
            .lock()
            .observe(&patch, &canonical_operations, &before, &after)
        {
            NoOpPatchDecision::Execute => {
                self.metrics_collector().record_patch_no_op(PatchNoOpOutcome::Execute);
            }
            NoOpPatchDecision::Success { signature, occurrence } => {
                let outcome = if occurrence == 1 {
                    PatchNoOpOutcome::NoOpFirst
                } else {
                    PatchNoOpOutcome::NoOpRepeated
                };
                self.metrics_collector().record_patch_no_op(outcome);
                tracing::info!(
                    operation = tools::APPLY_PATCH,
                    decision = outcome.as_str(),
                    occurrence,
                    session_id = %context.session_id,
                    task_id = %context.task_id.as_deref().unwrap_or(""),
                    "apply_patch no-op decision"
                );
                let mut response = json!({
                    "success": true,
                    "no_op": true,
                    "occurrence": occurrence,
                    "signature": signature,
                    "message": "No files were changed because the requested final bytes already match the current files."
                });
                if occurrence == 2
                    && let Some(object) = response.as_object_mut()
                {
                    object.insert("warning".into(), json!(true));
                    object.insert("retry_prohibited".into(), json!(true));
                    object.insert(
                        "next_action".into(),
                        json!("Do not retry this same patch payload. Reread the files or submit a materially different patch."),
                    );
                }
                return Ok(response);
            }
            NoOpPatchDecision::Block { signature, occurrence } => {
                self.metrics_collector().record_patch_no_op(PatchNoOpOutcome::NoOpBlocked);
                tracing::info!(
                    operation = tools::APPLY_PATCH,
                    decision = PatchNoOpOutcome::NoOpBlocked.as_str(),
                    occurrence,
                    session_id = %context.session_id,
                    task_id = %context.task_id.as_deref().unwrap_or(""),
                    "apply_patch no-op decision"
                );
                return Ok(repeated_no_op_error(signature, occurrence));
            }
        }
        let diff = self.patch_diff_previews(&planned_writes);
        let results = patch.apply(&self.workspace_root_owned()).await?;
        for write in planned_writes {
            let (path, result) = match write {
                PlannedPatchWrite::Text { path, content, .. } => {
                    let result = self.edited_file_monitor_ref().record_agent_write_text(&path, &content);
                    (path, result)
                }
                PlannedPatchWrite::Removal { path, .. } => {
                    let result = self.edited_file_monitor_ref().record_agent_removal(&path);
                    (path, result)
                }
            };

            if let Err(err) = result {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Failed to refresh edited-file snapshot after apply_patch"
                );
            }
        }

        let mut response = json!({
            "success": true,
            "applied": results,
            "modified_files": mutation_paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        });
        response["diff"] = Value::Array(diff);
        Ok(response)
    }

    /// Canonicalise and deduplicate every path the patch can touch.
    ///
    /// Includes move and copy destinations, so leases and snapshots cover the
    /// destination of a rename as well as its source.
    async fn patch_mutation_paths(&self, patch: &crate::tools::editing::Patch) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for path in crate::tools::apply_patch::patch_mutation_target_paths(patch) {
            let path = path.to_str().ok_or_else(|| anyhow!("apply_patch path is not valid UTF-8"))?;
            paths.push(self.file_ops_tool().normalize_user_path(path).await?);
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Resolve each operation's source and destination to workspace paths.
    ///
    /// The guard hashes these rather than the raw patch text, so transport and
    /// path aliases describe the same operation. A move to the same path is
    /// recorded as having no destination.
    async fn canonical_patch_operations(
        &self,
        patch: &crate::tools::editing::Patch,
    ) -> Result<Vec<CanonicalPatchOperation>> {
        let mut canonical = Vec::with_capacity(patch.operations().len());
        for operation in patch.operations() {
            let (source, destination) = match operation {
                crate::tools::editing::PatchOperation::AddFile { path, .. }
                | crate::tools::editing::PatchOperation::DeleteFile { path } => {
                    (self.file_ops_tool().normalize_user_path(path).await?, None)
                }
                crate::tools::editing::PatchOperation::UpdateFile { path, new_path, .. } => {
                    let source = self.file_ops_tool().normalize_user_path(path).await?;
                    let destination = match new_path.as_deref().filter(|candidate| *candidate != path) {
                        Some(path) => Some(self.file_ops_tool().normalize_user_path(path).await?),
                        None => None,
                    };
                    (source, destination)
                }
            };
            canonical.push(CanonicalPatchOperation { source, destination });
        }
        Ok(canonical)
    }

    /// Plan every write the patch will perform, in applicator order.
    async fn planned_patch_writes(
        &self,
        patch: &crate::tools::editing::Patch,
        captured_paths: &[CapturedPatchPath],
    ) -> Result<Vec<PlannedPatchWrite>> {
        let mut writes = Vec::new();
        // Mirror the applicator, which replays operations in order against the
        // file state produced by the preceding operation. Planning every
        // operation from the pre-patch snapshot instead would let a patch that
        // touches one path twice look like a no-op and be reported as success
        // without the productive operations ever running.
        let mut pending_content: HashMap<PathBuf, String> = HashMap::new();
        for operation in patch.operations() {
            writes.extend(
                self.planned_patch_writes_for_operation(operation, captured_paths, &mut pending_content)
                    .await?,
            );
        }
        Ok(writes)
    }

    /// Build the bounded per-file diff previews returned to the model.
    fn patch_diff_previews(&self, writes: &[PlannedPatchWrite]) -> Vec<Value> {
        writes
            .iter()
            .map(|write| {
                let (path, before, after) = match write {
                    PlannedPatchWrite::Text { path, before, content } => (path, before.as_deref(), content.as_str()),
                    PlannedPatchWrite::Removal { path, before } => (path, before.as_deref(), ""),
                };
                let operation = match write {
                    PlannedPatchWrite::Text { before: None, .. } => "created",
                    PlannedPatchWrite::Text { before: Some(_), .. } => "updated",
                    PlannedPatchWrite::Removal { .. } => "deleted",
                };
                let relative_path = path
                    .strip_prefix(self.workspace_root())
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned();
                let mut preview = crate::tools::file_ops::build_diff_preview(&relative_path, before, after);
                if let Some(fields) = preview.as_object_mut() {
                    fields.insert("path".to_string(), Value::String(relative_path));
                    fields.insert("operation".to_string(), Value::String(operation.to_string()));
                }
                preview
            })
            .collect()
    }

    /// Snapshot every mutation path before any planning or mutation happens.
    ///
    /// These snapshots feed both the `expected_content_hash` precondition and
    /// the no-op guard's `before` state, so they must be taken before the
    /// patch is applied.
    async fn capture_patch_paths(&self, mutation_paths: &[PathBuf]) -> Result<Vec<CapturedPatchPath>> {
        let mut captured = Vec::with_capacity(mutation_paths.len());
        for path in mutation_paths {
            captured.push(CapturedPatchPath {
                path: path.clone(),
                snapshot: crate::tools::edited_file_monitor::EditedFileMonitor::current_snapshot(path).await?,
            });
        }
        Ok(captured)
    }

    /// Derive the before and after path states the no-op guard compares.
    fn planned_patch_states(
        &self,
        captured_paths: &[CapturedPatchPath],
        planned_writes: &[PlannedPatchWrite],
    ) -> (Vec<PatchPathState>, Vec<PatchPathState>) {
        let mut before = captured_paths
            .iter()
            .map(|captured| {
                PatchPathState::new(captured.path.clone(), captured.snapshot.exists, captured.snapshot.sha256.clone())
            })
            .collect::<Vec<_>>();
        let mut after = before.clone();
        for write in planned_writes {
            let (path, exists, hash) = match write {
                PlannedPatchWrite::Text { path, content, .. } => {
                    (path, true, vtcode_commons::utils::calculate_sha256(content.as_bytes()))
                }
                PlannedPatchWrite::Removal { path, .. } => (path, false, String::new()),
            };
            if let Some(state) = after.iter_mut().find(|state| state.path == *path) {
                state.exists = exists;
                state.content_hash = hash;
            } else {
                after.push(PatchPathState::new(path.clone(), exists, hash));
            }
        }
        before.sort_by(|left, right| left.path.cmp(&right.path));
        after.sort_by(|left, right| left.path.cmp(&right.path));
        (before, after)
    }

    /// Take one mutation lease per path so local writers queue behind this patch.
    async fn acquire_patch_mutations(&self, mutation_paths: &[PathBuf]) -> Vec<MutationLease> {
        let mut leases = Vec::new();
        for path in mutation_paths {
            leases.push(self.edited_file_monitor_ref().acquire_mutation(path).await);
        }
        leases
    }

    /// Report an external change to a target before the patch is applied.
    async fn detect_patch_operation_conflict(
        &self,
        operation: &crate::tools::editing::PatchOperation,
        override_snapshot: Option<FileSnapshot>,
    ) -> Result<Option<crate::tools::edited_file_monitor::FileConflict>> {
        let monitor = self.edited_file_monitor_ref();
        match operation {
            crate::tools::editing::PatchOperation::AddFile { path, content } => {
                let canonical_path = self.file_ops_tool().normalize_user_path(path).await?;
                monitor
                    .detect_conflict(&canonical_path, Some(content.clone()), override_snapshot)
                    .await
            }
            crate::tools::editing::PatchOperation::DeleteFile { path } => {
                let canonical_path = self.file_ops_tool().normalize_user_path(path).await?;
                monitor
                    .detect_conflict(&canonical_path, Some(String::new()), override_snapshot)
                    .await
            }
            crate::tools::editing::PatchOperation::UpdateFile { path, chunks, .. } => {
                let canonical_path = self.file_ops_tool().normalize_user_path(path).await?;
                let intended_content = if let Some(content) = monitor.tracked_read_text(&canonical_path).await {
                    match crate::tools::editing::patch::render_patch_update_content(
                        &canonical_path,
                        &content,
                        chunks,
                        path,
                    )
                    .await
                    {
                        Ok(rendered) => Some(rendered),
                        Err(err) => {
                            log_patch_preview_render_failure(&canonical_path, &err);
                            None
                        }
                    }
                } else {
                    None
                };

                monitor
                    .detect_conflict(&canonical_path, intended_content, override_snapshot)
                    .await
            }
        }
    }

    /// Plan the writes for one operation against the previous operation's result.
    async fn planned_patch_writes_for_operation(
        &self,
        operation: &crate::tools::editing::PatchOperation,
        captured_paths: &[CapturedPatchPath],
        pending_content: &mut HashMap<PathBuf, String>,
    ) -> Result<Vec<PlannedPatchWrite>> {
        match operation {
            crate::tools::editing::PatchOperation::AddFile { path, content } => {
                let path = self.file_ops_tool().normalize_user_path(path).await?;
                pending_content.insert(path.clone(), content.clone());
                Ok(vec![PlannedPatchWrite::Text { path, before: None, content: content.clone() }])
            }
            crate::tools::editing::PatchOperation::DeleteFile { path } => {
                let path = self.file_ops_tool().normalize_user_path(path).await?;
                let before = match pending_content.remove(&path) {
                    Some(content) => Some(content),
                    None => fs::read_to_string(&path).await.ok(),
                };
                Ok(vec![PlannedPatchWrite::Removal { path, before }])
            }
            crate::tools::editing::PatchOperation::UpdateFile { path, new_path, chunks } => {
                let canonical_path = self.file_ops_tool().normalize_user_path(path).await?;
                let source_content = match pending_content.get(&canonical_path) {
                    Some(content) => content.clone(),
                    None => {
                        let snapshot = captured_paths
                            .iter()
                            .find(|captured| captured.path == canonical_path)
                            .map(|captured| &captured.snapshot)
                            .ok_or_else(|| anyhow!("Missing captured patch state for {}", canonical_path.display()))?;
                        if !snapshot.exists {
                            return Err(anyhow!(crate::tools::editing::PatchError::MissingFile {
                                path: canonical_path.display().to_string(),
                            }));
                        }
                        snapshot.text_content.clone().ok_or_else(|| {
                            anyhow!(
                                "Failed to read patch source content for {}: file is not valid UTF-8",
                                canonical_path.display()
                            )
                        })?
                    }
                };

                let rendered = crate::tools::editing::patch::render_patch_update_content(
                    &canonical_path,
                    &source_content,
                    chunks,
                    path,
                )
                .await
                .map_err(|err| anyhow!("Failed to plan patch output for {}: {err}", canonical_path.display()))?;

                let mut writes = Vec::new();
                if let Some(destination) = new_path.as_ref().filter(|candidate| candidate.as_str() != path.as_str()) {
                    let destination = self.file_ops_tool().normalize_user_path(destination).await?;
                    pending_content.remove(&canonical_path);
                    pending_content.insert(destination.clone(), rendered.clone());
                    writes.push(PlannedPatchWrite::Removal { path: canonical_path, before: Some(source_content) });
                    writes.push(PlannedPatchWrite::Text { path: destination, before: None, content: rendered });
                } else {
                    pending_content.insert(canonical_path.clone(), rendered.clone());
                    writes.push(PlannedPatchWrite::Text {
                        path: canonical_path,
                        before: Some(source_content),
                        content: rendered,
                    });
                }

                Ok(writes)
            }
        }
    }
}

/// Log a conflict-preview render failure without reproducing patch text.
///
/// `PatchError`'s `Display` embeds the failing context or snippet, so the
/// `error = %err` shortcut would copy user patch text into the tracing stream.
/// Log the fixed reason code and the embedded text's byte length instead. The
/// render helper wraps its `PatchError` in `anyhow`, so recover it by downcast
/// rather than formatting the chain.
fn log_patch_preview_render_failure(path: &std::path::Path, error: &anyhow::Error) {
    let patch_error = error
        .downcast_ref::<crate::tools::editing::PatchError>()
        .or_else(|| error.root_cause().downcast_ref::<crate::tools::editing::PatchError>());
    let (reason, text_bytes) = match patch_error {
        Some(error) => (error.reason_code(), error.embedded_text_bytes()),
        None => ("patch_render_failed", 0),
    };
    tracing::debug!(
        path = %path.display(),
        reason,
        text_bytes,
        "Failed to render patch conflict preview content"
    );
}

#[cfg(test)]
mod tests {
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

        let output =
            String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8");
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
        let on_disk_hash =
            format!("sha256:{}", vtcode_commons::utils::calculate_sha256(format!("{MARKER}\n").as_bytes()));

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
        let output =
            String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8");
        assert!(output.contains("apply_patch no-op decision"), "the no-op decision must be logged: {output}");
        assert!(output.contains("content_hash_mismatch"), "the rejection outcome must be logged: {output}");
        assert!(!output.contains(MARKER), "patch text and file names must not reach tracing: {output}");
        assert!(!output.contains(&signature), "the no-op signature must not reach tracing: {output}");
        assert!(!output.contains(&stale_hash), "expected hashes must not reach tracing: {output}");
        assert!(!output.contains(&on_disk_hash), "on-disk hashes must not reach tracing: {output}");
        Ok(())
    }
}
