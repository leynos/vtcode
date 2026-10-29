use super::ToolRegistry;
use super::patch_preconditions::{invalid_patch_precondition, repeated_no_op_error, validate_patch_content_hash};
use crate::tools::edited_file_monitor::{FileSnapshot, MutationLease, conflict_override_snapshot};
use crate::tools::registry::patch_guard::{CanonicalPatchOperation, NoOpPatchDecision, PatchPathState};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

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
    pub(super) async fn execute_apply_patch_internal(&self, args: Value) -> Result<Value> {
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
            if let Some(error) = validate_patch_content_hash(self, &patch, &expected_hash, &captured_paths).await? {
                return Ok(error);
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
            NoOpPatchDecision::Execute => {}
            NoOpPatchDecision::Success { signature, occurrence } => {
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

    async fn acquire_patch_mutations(&self, mutation_paths: &[PathBuf]) -> Vec<MutationLease> {
        let mut leases = Vec::new();
        for path in mutation_paths {
            leases.push(self.edited_file_monitor_ref().acquire_mutation(path).await);
        }
        leases
    }

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
                            tracing::debug!(
                                path = %canonical_path.display(),
                                error = %err,
                                "Failed to render patch conflict preview content"
                            );
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
