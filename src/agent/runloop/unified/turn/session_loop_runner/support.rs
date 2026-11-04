use super::archive::workspace_archive_label;
use crate::agent::runloop::ResumeSession;
use crate::agent::runloop::git::{DirtyWorktreeStatus, git_dirty_worktree_entries, workspace_relative_display};
use crate::agent::runloop::unified::overlay_prompt::{OverlayWaitOutcome, show_overlay_and_wait};
use crate::agent::runloop::unified::turn::context::TurnLoopResult;
use anyhow::Result;
use diapason_config::loader::DiapasonConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::llm::provider::{AssistantPhase, MessageRole};
use diapason_core::tools::registry::ToolRegistry;
use diapason_core::utils::session_archive;
use diapason_core::utils::session_archive::{SessionMessage, SessionProgressArgs};
use diapason_ui::tui::app::{
    InlineHandle, InlineListItem, InlineListSelection, InlineSession, ListOverlayRequest, TransientRequest,
    TransientSubmission,
};
use std::sync::Arc;
use tokio::sync::Notify;

const STARTUP_PLANNING_WORKFLOW_ENTER_ACTION: &str = "planning_active:start_enter";
const STARTUP_PLANNING_WORKFLOW_STAY_ACTION: &str = "planning_active:start_stay";

#[cfg(test)]
#[derive(Clone)]
pub(super) struct TurnHistoryCheckpoint {
    baseline_len: usize,
    #[cfg(debug_assertions)]
    prefix_fingerprint: u64,
}

#[cfg(test)]
impl TurnHistoryCheckpoint {
    pub(super) fn capture(history: &[diapason_core::llm::provider::Message]) -> Self {
        Self {
            baseline_len: history.len(),
            #[cfg(debug_assertions)]
            prefix_fingerprint: Self::prefix_fingerprint(history),
        }
    }

    pub(super) fn rollback(&self, history: &mut Vec<diapason_core::llm::provider::Message>) {
        #[cfg(debug_assertions)]
        self.assert_append_only(history);
        history.truncate(self.baseline_len);
    }

    #[cfg(debug_assertions)]
    fn assert_append_only(&self, history: &[diapason_core::llm::provider::Message]) {
        debug_assert!(
            history.len() >= self.baseline_len,
            "turn history rollback requires append-only growth after checkpoint"
        );
        debug_assert_eq!(
            Self::prefix_fingerprint(&history[..self.baseline_len]),
            self.prefix_fingerprint,
            "turn history rollback requires the pre-checkpoint prefix to remain unchanged"
        );
    }

    #[cfg(debug_assertions)]
    fn prefix_fingerprint(history: &[diapason_core::llm::provider::Message]) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(history).unwrap_or_default().hash(&mut hasher);
        hasher.finish()
    }
}

pub(super) fn remove_transient_system_notes(
    history: &mut Vec<diapason_core::llm::provider::Message>,
    notes: &[String],
) {
    for note in notes.iter().rev() {
        if let Some(index) = history
            .iter()
            .rposition(|message| message.role == MessageRole::System && message.content.as_text() == note.as_str())
        {
            let _ = history.remove(index);
        }
    }
}

pub(super) fn build_tracked_file_freshness_note(
    workspace: &std::path::Path,
    stale_paths: &[std::path::PathBuf],
) -> Option<String> {
    if stale_paths.is_empty() {
        return None;
    }

    let display_paths = stale_paths
        .iter()
        .map(|path| format!("- {}", workspace_relative_display(workspace, path)))
        .collect::<Vec<_>>()
        .join("\n");

    Some(format!(
        "Freshness note: the following files changed on disk after Diapason last read them:\n{display_paths}\nRe-read these files before relying on earlier content because disk content is newer than the agent's prior read snapshot."
    ))
}

pub(super) fn format_workspace_relative_paths<I>(workspace: &std::path::Path, paths: I) -> String
where
    I: IntoIterator,
    I::Item: AsRef<std::path::Path>,
{
    let display_paths = paths
        .into_iter()
        .map(|path| workspace_relative_display(workspace, path.as_ref()))
        .collect::<Vec<_>>();
    if display_paths.is_empty() {
        return "none recorded".to_string();
    }

    display_paths.join(", ")
}

pub(super) fn build_unrelated_dirty_worktree_note(
    workspace: &std::path::Path,
    agent_touched_paths: &std::collections::BTreeSet<std::path::PathBuf>,
) -> Result<Option<String>> {
    let Some(entries) = git_dirty_worktree_entries(workspace)? else {
        return Ok(None);
    };

    let display_paths = entries
        .into_iter()
        .filter(|entry| entry.status == DirtyWorktreeStatus::Modified && !agent_touched_paths.contains(&entry.path))
        .map(|entry| format!("- {}", workspace_relative_display(workspace, &entry.path)))
        .collect::<Vec<_>>();

    if display_paths.is_empty() {
        return Ok(None);
    }

    Ok(Some(format!(
        "Workspace note: the following files already have unrelated user modifications before this turn:\n{}\nTreat these files as user-owned changes. Do not edit, format, revert, or overwrite them unless the user explicitly asks to work on those files.",
        display_paths.join("\n")
    )))
}

/// Append transient system notes for the upcoming turn.
///
/// `unrelated_dirty_note` is pre-fetched by the caller via `spawn_blocking`
/// (see `orchestration.rs`) because `build_unrelated_dirty_worktree_note`
/// spawns blocking `git` subprocesses — see the `# Blocking` docs in `git.rs`.
pub(super) fn append_transient_turn_notes(
    history: &mut Vec<diapason_core::llm::provider::Message>,
    workspace: &std::path::Path,
    tool_registry: &ToolRegistry,
    unrelated_dirty_note: Option<String>,
) -> Vec<String> {
    let mut transient_system_notes = Vec::with_capacity(2);

    if let Some(note) = {
        let stale_paths = tool_registry.edited_file_monitor_ref().stale_tracked_paths();
        build_tracked_file_freshness_note(workspace, &stale_paths)
    } {
        transient_system_notes.push(note.clone());
        history.push(diapason_core::llm::provider::Message::system(note));
    }

    if let Some(note) = unrelated_dirty_note {
        transient_system_notes.push(note.clone());
        history.push(diapason_core::llm::provider::Message::system(note));
    }

    transient_system_notes
}

pub(super) fn latest_assistant_result_text(messages: &[diapason_core::llm::provider::Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        if message.role != MessageRole::Assistant
            || message.tool_calls.is_some()
            || message.phase == Some(AssistantPhase::Commentary)
        {
            return None;
        }

        let text = message.content.as_text();
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExecutionSummaryStatus {
    Completed,
    Blocked,
    Failed,
}

impl ExecutionSummaryStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExecutionSummary {
    pub(super) status: ExecutionSummaryStatus,
    pub(super) blocker: Option<String>,
}

pub(super) fn classify_execution_summary(
    result: &TurnLoopResult,
    final_response_was_fallback: bool,
    checklist: Option<&serde_json::Value>,
    changed_files: bool,
) -> ExecutionSummaryStatus {
    match result {
        TurnLoopResult::Completed { .. } => {
            if final_response_was_fallback || !changed_files {
                return ExecutionSummaryStatus::Blocked;
            }
            let Some(checklist) = checklist else {
                return ExecutionSummaryStatus::Blocked;
            };
            let total = checklist.get("total").and_then(serde_json::Value::as_u64).unwrap_or_default();
            let completed = checklist
                .get("completed")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let pending = checklist.get("pending").and_then(serde_json::Value::as_u64).unwrap_or_default();
            let in_progress = checklist
                .get("in_progress")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let blocked = checklist.get("blocked").and_then(serde_json::Value::as_u64).unwrap_or_default();
            if total > 0 && completed >= total && pending == 0 && in_progress == 0 && blocked == 0 {
                ExecutionSummaryStatus::Completed
            } else {
                ExecutionSummaryStatus::Blocked
            }
        }
        TurnLoopResult::Blocked { .. } => ExecutionSummaryStatus::Blocked,
        TurnLoopResult::Aborted | TurnLoopResult::Cancelled | TurnLoopResult::Exit => ExecutionSummaryStatus::Failed,
    }
}

fn pending_checklist_items(checklist: &serde_json::Value) -> Vec<String> {
    checklist
        .get("items")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("status").and_then(serde_json::Value::as_str) != Some("completed"))
        .filter_map(|item| item.get("description").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .take(4)
        .collect()
}

fn execution_summary_blocker(
    result: &TurnLoopResult,
    final_response_was_fallback: bool,
    checklist: Option<&serde_json::Value>,
    changed_files: bool,
) -> Option<String> {
    if final_response_was_fallback {
        return Some("recovery ended with a deterministic fallback and did not confirm the requested work".to_string());
    }
    if !changed_files && matches!(result, TurnLoopResult::Completed { .. }) {
        return Some(
            "the approved-plan turn produced no file changes, so implementation completion was not confirmed"
                .to_string(),
        );
    }
    if matches!(result, TurnLoopResult::Aborted | TurnLoopResult::Cancelled | TurnLoopResult::Exit) {
        return Some("the execution turn did not finish successfully".to_string());
    }
    if let TurnLoopResult::Blocked { reason } = result {
        return Some(reason.clone().unwrap_or_else(|| "the execution turn was blocked".to_string()));
    }
    let Some(checklist) = checklist else {
        return Some("the approved-plan task checklist was not available".to_string());
    };
    let pending = pending_checklist_items(checklist);
    if !pending.is_empty() {
        return Some(format!("pending checklist items: {}", pending.join(", ")));
    }
    Some("the approved-plan checklist is not fully completed".to_string())
}

pub(super) async fn approved_plan_execution_summary(
    tool_registry: &ToolRegistry,
    result: &TurnLoopResult,
    final_response_was_fallback: bool,
    changed_files: bool,
) -> ExecutionSummary {
    let checklist_result = match tool_registry.get_tool(diapason_core::config::constants::tools::TASK_TRACKER) {
        Some(tool) => match tool.execute(serde_json::json!({"action": "list"})).await {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!(error = %err, "Failed to read approved-plan task tracker for execution summary");
                None
            }
        },
        None => None,
    };
    let checklist = checklist_result.as_ref().and_then(|value| value.get("checklist"));
    let status = classify_execution_summary(result, final_response_was_fallback, checklist, changed_files);
    let blocker = (status != ExecutionSummaryStatus::Completed)
        .then(|| execution_summary_blocker(result, final_response_was_fallback, checklist, changed_files))
        .flatten();
    ExecutionSummary { status, blocker }
}

pub(super) fn take_pending_resumed_user_prompt(
    history: &mut Vec<diapason_core::llm::provider::Message>,
) -> Option<String> {
    let user_index = history.iter().rposition(|message| message.role == MessageRole::User)?;
    if history
        .iter()
        .skip(user_index + 1)
        .any(|message| message.role != MessageRole::System)
    {
        return None;
    }

    let prompt = history[user_index].content.as_text().trim().to_string();
    if prompt.is_empty() {
        return None;
    }

    let _ = history.remove(user_index);
    Some(prompt)
}

pub(super) fn prepare_resume_bootstrap_without_archive(
    resume: &ResumeSession,
    mut metadata: session_archive::SessionArchiveMetadata,
    reserved_archive_id: Option<String>,
) -> (diapason_core::core::threads::ThreadBootstrap, String) {
    let source_metadata = &resume.snapshot().metadata;
    let is_compatible = metadata.workspace_path == source_metadata.workspace_path
        && metadata.provider == source_metadata.provider
        && metadata.model == source_metadata.model;
    if is_compatible && let Some(lineage_id) = source_metadata.prompt_cache_lineage_id.as_ref() {
        metadata.prompt_cache_lineage_id = Some(lineage_id.clone());
    }
    metadata.continuation_metadata = source_metadata.continuation_metadata.clone();
    if resume.is_fork() {
        metadata.parent_session_id = Some(resume.identifier());
        metadata.fork_mode = Some(if resume.summarize_fork() {
            session_archive::SessionForkMode::Summarized
        } else {
            session_archive::SessionForkMode::FullCopy
        });
    }

    let mut bootstrap = resume.bootstrap().clone();
    bootstrap.metadata = Some(metadata);
    if resume.is_fork() {
        bootstrap.archive_listing = None;
    }

    let thread_id = match resume.intent() {
        diapason_core::core::threads::ArchivedSessionIntent::ResumeInPlace => resume.identifier(),
        diapason_core::core::threads::ArchivedSessionIntent::ForkNewArchive { .. } => reserved_archive_id
            .unwrap_or_else(|| {
                session_archive::generate_session_archive_identifier(
                    &workspace_archive_label(std::path::Path::new(&resume.snapshot().metadata.workspace_path)),
                    resume.custom_suffix().map(str::to_owned),
                )
            }),
    };

    (bootstrap, thread_id)
}

pub(super) async fn checkpoint_session_archive_start(
    archive: &session_archive::SessionArchive,
    thread_handle: &diapason_core::core::threads::ThreadRuntimeHandle,
) -> Result<()> {
    let snapshot = thread_handle.snapshot();
    let messages: Vec<SessionMessage> = snapshot.messages.iter().map(SessionMessage::from).collect();
    archive
        .persist_progress_async(SessionProgressArgs {
            total_messages: snapshot.messages.len(),
            distinct_tools: Vec::new(),
            messages: messages.clone(),
            recent_messages: messages,
            turn_number: 1,
            token_usage: None,
            max_context_tokens: None,
            loaded_skills: Some(snapshot.loaded_skills),
            turn_diagnostics: None,
        })
        .await?;
    Ok(())
}

pub(super) async fn force_reload_workspace_config_for_execution(
    workspace: &std::path::Path,
    runtime_cfg: &CoreAgentConfig,
    vt_cfg: &mut Option<DiapasonConfig>,
    tool_registry: &mut ToolRegistry,
    async_mcp_manager: Option<&crate::agent::runloop::unified::async_mcp_manager::AsyncMcpManager>,
) -> Result<()> {
    crate::agent::runloop::unified::turn::workspace::refresh_vt_config(workspace, runtime_cfg, vt_cfg).await?;

    if let Some(cfg) = vt_cfg.as_ref() {
        crate::agent::runloop::unified::turn::workspace::apply_workspace_config_to_registry(tool_registry, cfg)?;

        if let Some(mcp_manager) = async_mcp_manager {
            let desired_policy =
                crate::agent::runloop::unified::async_mcp_manager::approval_policy_from_human_in_the_loop(
                    cfg.security.human_in_the_loop,
                );
            if mcp_manager.approval_policy() != desired_policy {
                mcp_manager.set_approval_policy(desired_policy);
            }
        }
    }

    Ok(())
}

pub(super) async fn prompt_startup_planning_workflow(
    handle: &InlineHandle,
    session: &mut InlineSession,
    ctrl_c_state: &Arc<crate::agent::runloop::unified::state::CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
) -> Result<bool> {
    let overlay = TransientRequest::List(ListOverlayRequest {
        title: "Start planning workflow?".to_string(),
        lines: vec![
            "Your configuration starts new sessions in the planning workflow.".to_string(),
            "The planning workflow keeps mutating tools blocked until execution is approved.".to_string(),
        ],
        footer_hint: Some("You can start or finish planning later with `/plan`.".to_string()),
        items: vec![
            InlineListItem {
                title: "Start planning".to_string(),
                subtitle: Some("Use the planning workflow before execution.".to_string()),
                badge: Some("Recommended".to_string()),
                indent: 0,
                selection: Some(InlineListSelection::ConfigAction(STARTUP_PLANNING_WORKFLOW_ENTER_ACTION.to_string())),
                search_value: None,
            },
            InlineListItem {
                title: "Start normally".to_string(),
                subtitle: Some("Use the selected primary agent without planning first.".to_string()),
                badge: None,
                indent: 0,
                selection: Some(InlineListSelection::ConfigAction(STARTUP_PLANNING_WORKFLOW_STAY_ACTION.to_string())),
                search_value: None,
            },
        ],
        selected: Some(InlineListSelection::ConfigAction(STARTUP_PLANNING_WORKFLOW_ENTER_ACTION.to_string())),
        search: None,
        hotkeys: Vec::new(),
    });

    let outcome =
        show_overlay_and_wait(handle, session, overlay, ctrl_c_state, ctrl_c_notify, |submission| match submission {
            TransientSubmission::Selection(InlineListSelection::ConfigAction(action))
                if action == STARTUP_PLANNING_WORKFLOW_ENTER_ACTION =>
            {
                Some(true)
            }
            TransientSubmission::Selection(InlineListSelection::ConfigAction(action))
                if action == STARTUP_PLANNING_WORKFLOW_STAY_ACTION =>
            {
                Some(false)
            }
            TransientSubmission::Selection(_) => Some(false),
            _ => None,
        })
        .await?;

    Ok(matches!(outcome, OverlayWaitOutcome::Submitted(true)))
}

#[cfg(test)]
mod tests {
    use super::{ExecutionSummaryStatus, classify_execution_summary};
    use crate::agent::runloop::unified::turn::context::TurnLoopResult;
    use serde_json::json;

    #[test]
    fn pending_approved_plan_checklist_cannot_be_completed() {
        let checklist = json!({
            "total": 3,
            "completed": 1,
            "pending": 1,
            "in_progress": 1,
            "blocked": 0
        });
        let result = TurnLoopResult::Completed { plan_approved_execution_pending: false };

        assert_eq!(classify_execution_summary(&result, false, Some(&checklist), true), ExecutionSummaryStatus::Blocked);
    }

    #[test]
    fn completed_approved_plan_requires_all_checklist_items() {
        let checklist = json!({
            "total": 2,
            "completed": 2,
            "pending": 0,
            "in_progress": 0,
            "blocked": 0
        });
        let result = TurnLoopResult::Completed { plan_approved_execution_pending: false };

        assert_eq!(
            classify_execution_summary(&result, false, Some(&checklist), true),
            ExecutionSummaryStatus::Completed
        );
    }

    #[test]
    fn recovery_fallback_is_blocked_even_with_a_complete_tracker() {
        let checklist = json!({
            "total": 1,
            "completed": 1,
            "pending": 0,
            "in_progress": 0,
            "blocked": 0
        });
        let result = TurnLoopResult::Completed { plan_approved_execution_pending: false };

        assert_eq!(classify_execution_summary(&result, true, Some(&checklist), true), ExecutionSummaryStatus::Blocked);
    }

    #[test]
    fn completed_plan_without_file_changes_is_blocked() {
        let checklist = json!({
            "total": 1,
            "completed": 1,
            "pending": 0,
            "in_progress": 0,
            "blocked": 0
        });
        let result = TurnLoopResult::Completed { plan_approved_execution_pending: false };

        assert_eq!(
            classify_execution_summary(&result, false, Some(&checklist), false),
            ExecutionSummaryStatus::Blocked
        );
    }
}
