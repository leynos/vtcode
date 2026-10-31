//! Snapshot and render the file changes produced by ACP apply-patch calls.
use super::super::ZedAgent;
use crate::acp;
use crate::reports::{ToolExecutionReport, create_diff_content};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use vtcode_core::config::constants::tools;
use vtcode_core::tools::apply_patch::{Patch, PatchOperation, decode_apply_patch_input};

pub(super) struct ApplyPatchSnapshot {
    pub(super) files: Vec<ApplyPatchFileSnapshot>,
}

pub(super) struct ApplyPatchFileSnapshot {
    pub(super) path: PathBuf,
    pub(super) old_text: Option<String>,
    pub(super) new_text: ApplyPatchNewText,
}

pub(super) enum ApplyPatchNewText {
    Empty,
    ReadAfterWrite,
}

enum ValidatedApplyPatchOperation {
    Add {
        path: PathBuf,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        source_path: PathBuf,
        destination_path: Option<PathBuf>,
    },
}

async fn validate_apply_patch_path(workspace: &Path, path: &str) -> Option<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    vtcode_commons::paths::ensure_path_within_workspace_resolved(&workspace.join(relative_path), workspace)
        .await
        .ok()
}

pub(super) async fn capture_apply_patch_snapshot_for_workspace(
    workspace: &Path,
    patch: &Patch,
) -> Option<ApplyPatchSnapshot> {
    let mut validated_operations = Vec::with_capacity(patch.operations().len());
    for operation in patch.operations() {
        match operation {
            PatchOperation::AddFile { path, .. } => {
                validated_operations.push(ValidatedApplyPatchOperation::Add {
                    path: validate_apply_patch_path(workspace, path).await?,
                });
            }
            PatchOperation::DeleteFile { path } => {
                validated_operations.push(ValidatedApplyPatchOperation::Delete {
                    path: validate_apply_patch_path(workspace, path).await?,
                });
            }
            PatchOperation::UpdateFile { path, new_path, .. } => {
                let source_path = validate_apply_patch_path(workspace, path).await?;
                let destination_path = match new_path.as_deref() {
                    Some(destination) => {
                        let safe_destination = validate_apply_patch_path(workspace, destination).await?;
                        (destination != path.as_str()).then_some(safe_destination)
                    }
                    None => None,
                };
                validated_operations.push(ValidatedApplyPatchOperation::Update { source_path, destination_path });
            }
        }
    }

    let mut files = Vec::new();

    for operation in validated_operations {
        match operation {
            ValidatedApplyPatchOperation::Add { path } => files.push(ApplyPatchFileSnapshot {
                path,
                old_text: None,
                new_text: ApplyPatchNewText::ReadAfterWrite,
            }),
            ValidatedApplyPatchOperation::Delete { path } => files.push(ApplyPatchFileSnapshot {
                old_text: Some(tokio::fs::read_to_string(&path).await.ok()?),
                path,
                new_text: ApplyPatchNewText::Empty,
            }),
            ValidatedApplyPatchOperation::Update { source_path, destination_path } => {
                let old_text = tokio::fs::read_to_string(&source_path).await.ok()?;
                files.push(ApplyPatchFileSnapshot {
                    path: source_path,
                    old_text: Some(old_text),
                    new_text: if destination_path.is_some() {
                        ApplyPatchNewText::Empty
                    } else {
                        ApplyPatchNewText::ReadAfterWrite
                    },
                });
                if let Some(destination_path) = destination_path {
                    files.push(ApplyPatchFileSnapshot {
                        path: destination_path,
                        old_text: None,
                        new_text: ApplyPatchNewText::ReadAfterWrite,
                    });
                }
            }
        }
    }

    Some(ApplyPatchSnapshot { files })
}

pub(super) async fn attach_apply_patch_diff_content(
    report: &mut ToolExecutionReport,
    snapshot: Option<&ApplyPatchSnapshot>,
) {
    if !matches!(report.status, acp::ToolCallStatus::Completed) {
        return;
    }
    let Some(snapshot) = snapshot else {
        return;
    };
    let Some(content) = render_apply_patch_diff_content(snapshot).await else {
        return;
    };
    report.content = content;
}

pub(super) async fn render_apply_patch_diff_content(
    snapshot: &ApplyPatchSnapshot,
) -> Option<Vec<acp::ToolCallContent>> {
    let mut content = Vec::with_capacity(snapshot.files.len());
    for file in &snapshot.files {
        let new_text = match file.new_text {
            ApplyPatchNewText::Empty => String::new(),
            ApplyPatchNewText::ReadAfterWrite => tokio::fs::read_to_string(&file.path).await.ok()?,
        };
        content.push(create_diff_content(file.path.to_string_lossy().as_ref(), file.old_text.as_deref(), &new_text));
    }
    Some(content)
}

impl ZedAgent {
    pub(super) fn initial_tool_content(tool_name: &str, args: Option<&Value>) -> Vec<acp::ToolCallContent> {
        if tool_name != tools::APPLY_PATCH {
            return Vec::new();
        }

        args.and_then(|args| decode_apply_patch_input(args).ok().flatten())
            .map(|patch| vec![acp::ToolCallContent::from(patch.text)])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::ZedAgent;
    use super::super::execution::append_cancellation_after_completion_note;
    use super::*;
    use assert_fs::TempDir;
    use serde_json::json;
    use std::fs;
    use vtcode_core::llm::provider::ToolCall as ProviderToolCall;

    #[test]
    fn apply_patch_call_exposes_patch_text_before_execution() {
        let patch = "*** Begin Patch\n*** Add File: visible.txt\n+hello\n*** End Patch\n";
        let content = ZedAgent::initial_tool_content(tools::APPLY_PATCH, Some(&json!({ "patch": patch })));

        let [acp::ToolCallContent::Content(content)] = content.as_slice() else {
            panic!("apply_patch should expose one text content block");
        };
        let acp::ContentBlock::Text(text) = &content.content else {
            panic!("apply_patch preview should be text");
        };
        assert_eq!(text.text, patch);
    }

    #[test]
    fn other_tool_calls_do_not_echo_raw_arguments_as_content() {
        assert!(ZedAgent::initial_tool_content(tools::EXEC_COMMAND, Some(&json!({ "command": "true" }))).is_empty());
    }

    #[tokio::test]
    async fn apply_patch_diff_content_covers_update_add_delete_move_and_multi_file() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("update.txt"), "old update").unwrap();
        fs::write(temp.path().join("delete.txt"), "old delete").unwrap();
        fs::write(temp.path().join("move.txt"), "old move").unwrap();
        let patch = Patch::parse(
            "*** Begin Patch\n*** Update File: update.txt\n@@\n-old update\n+new update\n*** Add File: add.txt\n+new add\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: moved.txt\n@@\n-old move\n+new move\n*** End Patch\n",
        )
        .unwrap();

        let snapshot = capture_apply_patch_snapshot_for_workspace(temp.path(), &patch).await.unwrap();
        assert_eq!(snapshot.files.len(), 5);
        assert!(snapshot.files.iter().all(|file| file.path.is_absolute()));
        assert_eq!(snapshot.files[0].old_text.as_deref(), Some("old update"));
        assert_eq!(snapshot.files[1].old_text, None);
        assert_eq!(snapshot.files[2].old_text.as_deref(), Some("old delete"));
        assert_eq!(snapshot.files[3].old_text.as_deref(), Some("old move"));
        assert_eq!(snapshot.files[4].old_text, None);

        fs::write(temp.path().join("update.txt"), "new update").unwrap();
        fs::write(temp.path().join("add.txt"), "new add").unwrap();
        fs::remove_file(temp.path().join("delete.txt")).unwrap();
        fs::remove_file(temp.path().join("move.txt")).unwrap();
        fs::write(temp.path().join("moved.txt"), "new move").unwrap();

        let content = render_apply_patch_diff_content(&snapshot).await.unwrap();
        assert_eq!(content.len(), 5);
        let diffs = content
            .iter()
            .map(|block| {
                let acp::ToolCallContent::Diff(diff) = block else {
                    panic!("expected ACP diff content");
                };
                diff
            })
            .collect::<Vec<_>>();
        assert_eq!(diffs[0].old_text.as_deref(), Some("old update"));
        assert_eq!(diffs[0].new_text, "new update");
        assert_eq!(diffs[1].old_text, None);
        assert_eq!(diffs[1].new_text, "new add");
        assert_eq!(diffs[2].old_text.as_deref(), Some("old delete"));
        assert_eq!(diffs[2].new_text, "");
        assert_eq!(diffs[3].old_text.as_deref(), Some("old move"));
        assert_eq!(diffs[3].new_text, "");
        assert_eq!(diffs[4].old_text, None);
        assert_eq!(diffs[4].new_text, "new move");
        assert_eq!(diffs[0].path, temp.path().join("update.txt"));
        assert_eq!(diffs[4].path, temp.path().join("moved.txt"));
    }

    #[tokio::test]
    async fn apply_patch_path_validation_rejects_parent_traversal() {
        let workspace = TempDir::new().expect("workspace for traversal rejection");
        assert!(validate_apply_patch_path(workspace.path(), "../secret.txt").await.is_none());
    }

    #[tokio::test]
    async fn apply_patch_snapshot_rejects_absolute_paths() {
        let workspace = TempDir::new().expect("workspace for absolute-path rejection");
        let outside = TempDir::new().expect("outside directory for absolute-path rejection");
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "outside secret").expect("write outside file");
        let patch =
            Patch::parse(&format!("*** Begin Patch\n*** Delete File: {}\n*** End Patch\n", outside_file.display()))
                .expect("parse patch with absolute target");

        assert!(
            capture_apply_patch_snapshot_for_workspace(workspace.path(), &patch)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn apply_patch_snapshot_rejects_absolute_move_destinations() {
        let workspace = TempDir::new().expect("workspace for absolute move destination");
        let outside = TempDir::new().expect("outside directory for absolute move destination");
        fs::write(workspace.path().join("source.txt"), "old content").expect("write source file");
        let patch = Patch::parse(&format!(
            "*** Begin Patch\n*** Update File: source.txt\n*** Move to: {}\n@@\n-old content\n+new content\n*** End Patch\n",
            outside.path().join("destination.txt").display()
        ))
        .expect("parse patch with absolute move destination");

        assert!(
            capture_apply_patch_snapshot_for_workspace(workspace.path(), &patch)
                .await
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_patch_snapshot_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().expect("workspace for symlink rejection");
        let outside = TempDir::new().expect("outside directory for symlink rejection");
        fs::write(outside.path().join("secret.txt"), "outside secret").expect("write outside file");
        symlink(outside.path(), workspace.path().join("escape")).expect("create symlink escape");
        let patch = Patch::parse("*** Begin Patch\n*** Delete File: escape/secret.txt\n*** End Patch\n")
            .expect("parse patch with symlink target");

        assert!(
            capture_apply_patch_snapshot_for_workspace(workspace.path(), &patch)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn apply_patch_snapshot_accepts_valid_relative_paths() {
        let workspace = TempDir::new().expect("workspace for valid relative path");
        fs::create_dir(workspace.path().join("nested")).expect("create nested workspace directory");
        fs::write(workspace.path().join("nested/valid.txt"), "workspace content").expect("write workspace file");
        let patch = Patch::parse("*** Begin Patch\n*** Delete File: nested/valid.txt\n*** End Patch\n")
            .expect("parse patch with valid relative target");

        let snapshot = capture_apply_patch_snapshot_for_workspace(workspace.path(), &patch)
            .await
            .expect("capture snapshot for valid relative path");
        let [file] = snapshot.files.as_slice() else {
            panic!("valid one-file patch should produce one snapshot");
        };
        assert_eq!(file.path, workspace.path().join("nested/valid.txt"));
        assert_eq!(file.old_text.as_deref(), Some("workspace content"));
    }

    #[tokio::test]
    async fn failed_or_cancelled_apply_patch_does_not_attach_diff_content() {
        let snapshot = ApplyPatchSnapshot { files: Vec::new() };

        let mut failed = ToolExecutionReport::failure(tools::APPLY_PATCH, "failed");
        let failed_content = failed.content.clone();
        attach_apply_patch_diff_content(&mut failed, Some(&snapshot)).await;
        assert_eq!(failed.content, failed_content);

        let mut cancelled = ToolExecutionReport::cancelled(tools::APPLY_PATCH);
        let cancelled_content = cancelled.content.clone();
        attach_apply_patch_diff_content(&mut cancelled, Some(&snapshot)).await;
        assert_eq!(cancelled.content, cancelled_content);
    }

    #[tokio::test]
    async fn cancellation_after_completed_apply_patch_preserves_result_audit_and_diff() {
        let temp = TempDir::new().expect("workspace for completed patch report");
        let diff_path = temp.path().join("completed.txt");
        let snapshot = ApplyPatchSnapshot {
            files: vec![ApplyPatchFileSnapshot {
                path: diff_path.clone(),
                old_text: Some("before".to_owned()),
                new_text: ApplyPatchNewText::Empty,
            }],
        };
        let mut report = ToolExecutionReport::success(
            Vec::new(),
            Vec::new(),
            json!({ "status": "success", "tool": tools::APPLY_PATCH }),
        );

        // Model the cancellation arriving after execute_resolved_tool returned
        // its completed report, before the ACP result is finalized.
        append_cancellation_after_completion_note(&mut report, true);
        attach_apply_patch_diff_content(&mut report, Some(&snapshot)).await;

        assert_eq!(report.status, acp::ToolCallStatus::Completed);
        assert_eq!(report.audit_status, vtcode_safety::audit_log::ToolAuditStatus::Success);
        assert!(
            report
                .llm_response
                .contains("The turn was cancelled after this tool completed; its effects were applied.")
        );

        let update = ZedAgent::update_fields_from_report(&report);
        assert_eq!(update.status, Some(acp::ToolCallStatus::Completed));
        let [acp::ToolCallContent::Diff(diff)] = update.content.as_deref().expect("completed patch diff") else {
            panic!("completed patch result should retain its structured diff");
        };
        assert_eq!(diff.path, diff_path);
        assert_eq!(diff.old_text.as_deref(), Some("before"));
        assert_eq!(diff.new_text, "");

        let result = ZedAgent::tool_call_result_from_report(
            &ProviderToolCall::function("completed-call".to_owned(), tools::APPLY_PATCH.to_owned(), "{}".to_owned()),
            report,
        );
        assert_eq!(result.audit_status, vtcode_safety::audit_log::ToolAuditStatus::Success);
        assert!(
            result
                .llm_response
                .contains("The turn was cancelled after this tool completed; its effects were applied.")
        );
    }
}
