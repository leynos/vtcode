//! Patch write planning and conflict detection.
//!
//! Writes are planned in applicator order from the preceding operation's
//! *result* rather than from the pre-patch snapshot. That carry-forward keeps
//! the no-op guard honest when a patch updates the same path more than once:
//! planning from the original snapshot would make a productive chain look like
//! a no-op.

use super::CapturedPatchPath;
use super::log_patch_preview_render_failure;
use crate::tools::edited_file_monitor::FileSnapshot;
use crate::tools::registry::ToolRegistry;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

/// One file mutation the patch will perform, in applicator order.
pub(super) enum PlannedPatchWrite {
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

impl ToolRegistry {
    /// Report an external change to a target before the patch is applied.
    pub(super) async fn detect_patch_operation_conflict(
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
    pub(super) async fn planned_patch_writes_for_operation(
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
