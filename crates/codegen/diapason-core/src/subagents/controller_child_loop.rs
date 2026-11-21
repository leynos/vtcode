//! Runs and supervises the child-agent loop used by the subagent controller.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::future::{BoxFuture, select_all};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, RwLock};

use crate::config::DiapasonConfig;
use crate::config::models::ModelId;
use crate::config::types::ReasoningEffortLevel;
use crate::core::agent::runner::{AgentRunner, RunnerSettings};
use crate::core::agent::task::Task;
use crate::core::threads::{ThreadBootstrap, ThreadId, ThreadRuntimeHandle, ThreadSnapshot};
use crate::hooks::{LifecycleHookEngine, SessionStartTrigger};
use crate::llm::provider::Message;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::{PtyManager, PtySize};
use crate::utils::session_archive::{SessionArchive, SessionArchiveMetadata, find_session_by_identifier};
use diapason_config::SubagentSpec;
use diapason_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use super::TaskOwner;
use super::types::ChildRunRequest;
use diapason_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::*;

struct ChildExecutionContext {
    spec: SubagentSpec,
    session_id: String,
    parent_thread_id: String,
    bootstrap_messages: Vec<Message>,
    display_label: String,
    background: bool,
    worktree_path: Option<PathBuf>,
    existing_child_controller: Option<Arc<SubagentController>>,
}

struct PreparedChildExecution {
    spec: SubagentSpec,
    session_id: String,
    parent_session_id: String,
    prompt: String,
    display_label: String,
    background: bool,
    bootstrap_messages: Vec<Message>,
    effective_workspace: PathBuf,
    allow_nested_delegation: bool,
    existing_child_controller: Option<Arc<SubagentController>>,
    child_controller: Option<Arc<SubagentController>>,
    child_cfg: DiapasonConfig,
    child_reasoning_effort: ReasoningEffortLevel,
    archive_metadata: SessionArchiveMetadata,
    archive: SessionArchive,
    runner: Pin<Box<AgentRunner>>,
}

struct ResolvedChildRuntime {
    effective_workspace: PathBuf,
    allow_nested_delegation: bool,
    parent_session_id: String,
    resolved_model: ModelId,
    child_reasoning_effort: ReasoningEffortLevel,
    child_cfg: DiapasonConfig,
}

struct ChildArchiveRequest<'a> {
    session_id: &'a str,
    workspace: &'a Path,
    config: &'a DiapasonConfig,
    reasoning_effort: ReasoningEffortLevel,
    parent_session_id: &'a str,
    bootstrap_messages: &'a [Message],
}

struct PreparedChildArchive {
    metadata: SessionArchiveMetadata,
    bootstrap: ThreadBootstrap,
    archive: SessionArchive,
}

struct SubagentStopHookContext {
    parent_session_id: String,
    child_thread_id: String,
    agent_name: String,
    display_label: String,
    background: bool,
    status: String,
    transcript_path: Option<PathBuf>,
}

/// The runtime a prepared child run publishes onto its record.
///
/// These fields describe a runner that has started, so they travel together
/// and are published only once the run's controller attachment has been
/// accepted.
struct ChildRuntimePublication {
    archive_metadata: SessionArchiveMetadata,
    archive_path: PathBuf,
    effective_config: DiapasonConfig,
    thread_handle: ThreadRuntimeHandle,
    child_controller: Option<Arc<SubagentController>>,
}

impl SubagentController {
    pub(super) async fn launch_child(&self, child_id: &str) -> Result<()> {
        // Publish the queued state first, so an observer sees the position the
        // run is waiting in before the run can report itself running.
        {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            // A record a close already settled must not be resurrected. The
            // explicit closure outlives a settled `Completed`/`Failed` status,
            // so starting the task here would undo a cancellation whose caller
            // was already told it had succeeded.
            if record.status.is_terminal() {
                bail!("Subagent {child_id} is closed; refusing to launch it");
            }
            record.status = SubagentStatus::Queued;
            record.updated_at = Utc::now();
        }
        self.publish_child_status(child_id).await;

        // Spawn the task and publish its handle in one state-lock section. A
        // cancel takes the same lock, so it either lands before this section —
        // leaving the record closed, which refuses the launch — or after it,
        // where the handle is already published and is aborted with the
        // record. Releasing the lock in between would let a cancel find no
        // handle, mark the record closed and return success while the launcher
        // then started the task and dequeued the prompt back into `Running`.
        let mut state = self.state.write().await;
        let status = state
            .children
            .get(child_id)
            .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?
            .status;
        if status.is_terminal() {
            bail!("Subagent {child_id} was closed before it could start");
        }
        // The loop's first act is to take this same lock, so the task cannot
        // dequeue its prompt before its handle is published below.
        let controller = self.clone();
        let target = child_id.to_string();
        let handle = tokio::spawn(async move {
            Box::pin(controller.child_loop(&target)).await;

            // After child_loop completes, reconcile worktree if needed.
            // This runs on the owned controller clone so it does not affect
            // the Send-ness of child_loop's future.
            let worktree_info = {
                let state = controller.state.read().await;
                state
                    .children
                    .get(&target)
                    .and_then(|record| record.worktree_path.as_ref().map(|p| (p.clone(), record.spec.name.clone())))
            };

            if let Some((wt_path, wt_name)) = worktree_info
                && controller.config.vt_cfg.automation.loop_engine.reconcile_on_complete
            {
                controller.run_worktree_reconciliation(&target, &wt_path, &wt_name).await;
            }
        });
        let record = state
            .children
            .get_mut(child_id)
            .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
        record.handle = Some(handle);
        Ok(())
    }

    async fn child_loop(&self, child_id: &str) {
        loop {
            let Some(request) = self.dequeue_child_run(child_id).await else {
                return;
            };

            let result = self.run_child_once(child_id, request).await;
            let Some(has_more_work) = self.apply_child_run_result(child_id, result).await else {
                return;
            };
            if has_more_work {
                continue;
            }

            self.close_nested_subagents(child_id).await;
            self.mark_child_loop_idle(child_id).await;
            return;
        }
    }

    async fn dequeue_child_run(&self, child_id: &str) -> Option<ChildRunRequest> {
        let mut state = self.state.write().await;
        let record = state.children.get_mut(child_id)?;
        let request = record.dequeue_run();
        if request.is_none() {
            record.handle = None;
            record.updated_at = Utc::now();
        }
        request
    }

    async fn apply_child_run_result(&self, child_id: &str, result: Result<ChildRunResult>) -> Option<bool> {
        let (has_more_work, hook_payload, progress_entry) = {
            let mut state = self.state.write().await;
            let record = state.children.get_mut(child_id)?;
            record.updated_at = Utc::now();
            let has_more_work = record.apply_result(result);
            let hook_payload = (!has_more_work).then(|| {
                let (
                    parent_session_id,
                    child_thread_id,
                    agent_name,
                    display_label,
                    background,
                    status,
                    transcript_path,
                ) = record.build_hook_payload();
                SubagentStopHookContext {
                    parent_session_id,
                    child_thread_id,
                    agent_name,
                    display_label,
                    background,
                    status,
                    transcript_path,
                }
            });
            let progress_entry = record.build_status_entry();
            (has_more_work, hook_payload, progress_entry)
        };
        self.publish_subagent_progress(progress_entry).await;
        self.run_subagent_stop_hook(child_id, hook_payload).await;
        Some(has_more_work)
    }

    async fn run_subagent_stop_hook(&self, child_id: &str, payload: Option<SubagentStopHookContext>) {
        let Some(payload) = payload else {
            return;
        };
        let Some(hooks) = self.lifecycle_hooks.as_ref() else {
            return;
        };
        if let Err(error) = hooks
            .run_subagent_stop(
                &payload.parent_session_id,
                &payload.child_thread_id,
                &payload.agent_name,
                &payload.display_label,
                payload.background,
                &payload.status,
                payload.transcript_path.as_deref(),
            )
            .await
        {
            tracing::warn!(child_id, error = %error, "Failed to run subagent stop hooks");
        }
    }

    async fn close_nested_subagents(&self, child_id: &str) {
        // A terminal child owns its nested controller, so complete its lifecycle
        // before marking the child loop idle.
        let nested = {
            let state = self.state.read().await;
            state.children.get(child_id).and_then(|record| {
                record
                    .child_controller
                    .clone()
                    .map(|controller| (controller, record.session_id.clone()))
            })
        };
        if let Some((controller, session_id)) = nested {
            let ids = controller.spawn_child_ids_for_parent(&session_id).await;
            for id in ids {
                if let Err(err) = controller.close(TaskOwner::Workspace, &id).await {
                    tracing::warn!(child_id, node_id = id.as_str(), error = %err, "Failed to close nested subagent subtree on child completion");
                }
            }
        }
    }

    async fn mark_child_loop_idle(&self, child_id: &str) {
        let mut state = self.state.write().await;
        if let Some(record) = state.children.get_mut(child_id) {
            record.handle = None;
            record.updated_at = Utc::now();
        }
    }

    async fn begin_child_run(&self, child_id: &str) -> Result<ChildExecutionContext> {
        let context = {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            record.status = SubagentStatus::Running;
            record.updated_at = Utc::now();
            ChildExecutionContext {
                spec: record.spec.clone(),
                session_id: record.session_id.clone(),
                parent_thread_id: record.parent_thread_id.clone(),
                bootstrap_messages: record.stored_messages.clone(),
                display_label: record.display_label.clone(),
                background: record.background,
                worktree_path: record.worktree_path.clone(),
                existing_child_controller: record.child_controller.clone(),
            }
        };
        self.publish_child_status(child_id).await;
        Ok(context)
    }

    fn run_child_once<'a>(
        &'a self,
        child_id: &'a str,
        request: ChildRunRequest,
    ) -> BoxFuture<'a, Result<ChildRunResult>> {
        Box::pin(async move {
            let context = self.begin_child_run(child_id).await?;
            let mut execution = self.prepare_child_execution(context, request).await?;
            self.attach_nested_controller(child_id, &mut execution).await;
            let thread_handle = self.record_child_runtime(child_id, &execution).await?;
            self.run_child_start_hook(child_id, &execution, &thread_handle).await;
            self.execute_child_task(execution).await
        })
    }

    fn prepare_child_execution(
        &self,
        context: ChildExecutionContext,
        request: ChildRunRequest,
    ) -> BoxFuture<'_, Result<PreparedChildExecution>> {
        Box::pin(async move {
            let runtime = self.resolve_child_runtime(&context, &request).await?;
            let ChildExecutionContext {
                spec,
                session_id,
                bootstrap_messages,
                display_label,
                background,
                existing_child_controller,
                ..
            } = context;
            let ChildRunRequest { prompt, .. } = request;
            let archive = self
                .prepare_child_archive(ChildArchiveRequest {
                    session_id: session_id.as_str(),
                    workspace: &runtime.effective_workspace,
                    config: &runtime.child_cfg,
                    reasoning_effort: runtime.child_reasoning_effort,
                    parent_session_id: runtime.parent_session_id.as_str(),
                    bootstrap_messages: &bootstrap_messages,
                })
                .await?;
            let mut runner = Box::pin(
                AgentRunner::new_with_bootstrap(
                    agent_type_for_spec(&spec),
                    runtime.resolved_model,
                    self.config.api_key.clone(),
                    runtime.effective_workspace.clone(),
                    session_id.clone(),
                    RunnerSettings {
                        reasoning_effort: Some(runtime.child_reasoning_effort),
                        verbosity: None,
                    },
                    None,
                    archive.bootstrap,
                    Some(runtime.child_cfg.clone()),
                    self.config.openai_chatgpt_auth.clone(),
                )
                .await?,
            );
            runner.set_quiet(true);
            runner.set_subagent_mode(true);
            Ok(PreparedChildExecution {
                spec,
                session_id,
                parent_session_id: runtime.parent_session_id,
                prompt,
                display_label,
                background,
                bootstrap_messages,
                effective_workspace: runtime.effective_workspace,
                allow_nested_delegation: runtime.allow_nested_delegation,
                existing_child_controller,
                child_controller: None,
                child_cfg: runtime.child_cfg,
                child_reasoning_effort: runtime.child_reasoning_effort,
                archive_metadata: archive.metadata,
                archive: archive.archive,
                runner,
            })
        })
    }

    async fn resolve_child_runtime(
        &self,
        context: &ChildExecutionContext,
        request: &ChildRunRequest,
    ) -> Result<ResolvedChildRuntime> {
        // A worktree-isolated child executes and archives against its own root.
        let effective_workspace = context
            .worktree_path
            .clone()
            .unwrap_or_else(|| self.config.workspace_root.clone());
        let allow_nested_delegation = !context.spec.is_read_only()
            && self.config.depth.saturating_add(2) <= self.config.vt_cfg.subagents.max_depth;
        let (resolved_model, child_reasoning_effort, child_cfg) = prepare_child_runtime_config(
            &self.config.vt_cfg,
            &context.spec,
            self.config.parent_model.as_str(),
            self.config.parent_provider.as_str(),
            self.config.parent_reasoning_effort,
            request.max_turns,
            request.model_override.as_deref(),
            request.reasoning_override.as_deref(),
            allow_nested_delegation,
            resolve_effective_subagent_model,
        )?;
        Ok(ResolvedChildRuntime {
            effective_workspace,
            allow_nested_delegation,
            // Attribute the run to the record's own parent thread rather than
            // this controller's parent; a nested controller serves several
            // parent threads and must not re-attribute their children.
            parent_session_id: context.parent_thread_id.clone(),
            resolved_model,
            child_reasoning_effort,
            child_cfg,
        })
    }

    async fn prepare_child_archive(&self, request: ChildArchiveRequest<'_>) -> Result<PreparedChildArchive> {
        let metadata = build_subagent_archive_metadata(
            request.workspace,
            request.config.agent.default_model.as_str(),
            request.config.agent.provider.as_str(),
            request.config.agent.theme.as_str(),
            request.reasoning_effort.as_str(),
            request.parent_session_id,
            !request.bootstrap_messages.is_empty(),
        );
        let bootstrap = ThreadBootstrap::new(Some(metadata.clone())).with_messages(request.bootstrap_messages.to_vec());
        let archive = if let Some(listing) = find_session_by_identifier(request.session_id).await? {
            SessionArchive::resume_from_listing(&listing, metadata.clone())
        } else {
            SessionArchive::new_with_identifier(metadata.clone(), request.session_id.to_string()).await?
        };
        checkpoint_subagent_archive_start(&archive, request.bootstrap_messages).await?;
        Ok(PreparedChildArchive { metadata, bootstrap, archive })
    }

    async fn attach_nested_controller(&self, child_id: &str, execution: &mut PreparedChildExecution) {
        let child_controller = self.child_controller_for(child_id, execution).await;
        execution.child_controller = child_controller;
        if let Some(controller) = execution.child_controller.as_ref() {
            controller.set_parent_messages(&execution.bootstrap_messages).await;
            execution.runner.set_subagent_controller(controller.clone());
        }
    }

    /// Gives a newly created child controller the context it shares with its
    /// parent tree.
    ///
    /// The three inheritances are one unit: a child controller that adopted the
    /// parent's observers but kept its own allocator or locks would publish
    /// through one tree while reserving ids and ledger access through another.
    /// Production's only creator is [`Self::child_controller_for`], and tests
    /// exercise this same sequence rather than repeating it.
    pub(super) async fn inherit_tree_context(&mut self, parent: &SubagentController, parent_session_id: &str) {
        self.inherit_progress_observers(parent, parent_session_id).await;
        self.inherit_child_id_scope(parent);
        self.inherit_lock_scope(parent);
    }

    async fn child_controller_for(
        &self,
        child_id: &str,
        execution: &mut PreparedChildExecution,
    ) -> Option<Arc<SubagentController>> {
        if !execution.allow_nested_delegation {
            return None;
        }
        if let Some(controller) = execution.existing_child_controller.take() {
            return Some(controller);
        }

        // Child-scoped controllers keep grandchildren reachable across resumes
        // while preventing nested background subprocesses from escaping.
        let nested_config = SubagentControllerConfig {
            workspace_root: execution.effective_workspace.clone(),
            parent_session_id: execution.session_id.clone(),
            parent_model: execution.child_cfg.agent.default_model.clone(),
            parent_provider: execution.child_cfg.agent.provider.clone(),
            parent_reasoning_effort: execution.child_reasoning_effort,
            api_key: self.config.api_key.clone(),
            vt_cfg: execution.child_cfg.clone(),
            openai_chatgpt_auth: self.config.openai_chatgpt_auth.clone(),
            depth: self.config.depth.saturating_add(1),
            workspace_gated: self.config.workspace_gated,
            exec_sessions: self.config.exec_sessions.clone(),
            output_reader: None,
            pty_manager: self.config.pty_manager.clone(),
            managed_background_runtime: true,
            lock_scope: None,
        };
        match SubagentController::new(nested_config).await {
            Ok(mut controller) => {
                controller
                    .inherit_tree_context(self, execution.parent_session_id.as_str())
                    .await;
                Some(Arc::new(controller))
            }
            Err(err) => {
                // Fail closed: without a controller the child keeps the current
                // non-nested toolset and cannot delegate.
                tracing::warn!(
                    child_id,
                    error = %err,
                    "Failed to create nested subagent controller; child delegation disabled"
                );
                None
            }
        }
    }

    /// Returns how many child runtime captures this controller tree published.
    ///
    /// A test that must observe the capture itself cannot wait on the status
    /// change: the record is reopened and stamped before the resumed execution
    /// publishes its runtime, so the earlier configuration would satisfy a
    /// status-only wait.
    #[cfg(test)]
    pub(super) fn runtime_capture_count(&self) -> u64 {
        runtime_capture_witness::capture_count(&self.state)
    }

    async fn record_child_runtime(
        &self,
        child_id: &str,
        execution: &PreparedChildExecution,
    ) -> Result<ThreadRuntimeHandle> {
        let thread_handle = execution.runner.thread_handle();
        let mut state = self.state.write().await;
        Self::publish_child_runtime(
            &mut state,
            child_id,
            ChildRuntimePublication {
                archive_metadata: execution.archive_metadata.clone(),
                archive_path: execution.archive.path().to_path_buf(),
                effective_config: execution.child_cfg.clone(),
                thread_handle: thread_handle.clone(),
                child_controller: execution.child_controller.clone(),
            },
        )?;
        // A test that must observe this capture itself cannot rely on the
        // status change that precedes it: the record is reopened and stamped
        // before the resumed execution publishes its runtime, so the earlier
        // configuration would satisfy a status-only wait.
        #[cfg(test)]
        runtime_capture_witness::observe_capture(&self.state);
        Ok(thread_handle)
    }

    /// Publishes a child run's runtime onto its record.
    ///
    /// The controller attachment is validated first. A record that already
    /// names a different controller rejects it, and recording the archive and
    /// thread handle before that check would leave the record describing a
    /// runner that never started: the call fails, yet the record claims an
    /// archive path, an effective config and a live thread.
    fn publish_child_runtime(
        state: &mut ControllerState,
        child_id: &str,
        runtime: ChildRuntimePublication,
    ) -> Result<()> {
        if let Some(controller) = runtime.child_controller {
            Self::attach_child_controller_to_state(state, child_id, controller)?;
        }
        let record = state
            .children
            .get_mut(child_id)
            .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
        record.archive_metadata = Some(runtime.archive_metadata);
        record.archive_path = Some(runtime.archive_path);
        record.effective_config = Some(runtime.effective_config);
        record.thread_handle = Some(runtime.thread_handle);
        Ok(())
    }

    async fn run_child_start_hook(
        &self,
        child_id: &str,
        execution: &PreparedChildExecution,
        thread_handle: &ThreadRuntimeHandle,
    ) {
        if let Some(hooks) = self.lifecycle_hooks.as_ref()
            && let Err(err) = hooks
                .run_subagent_start(
                    execution.parent_session_id.as_str(),
                    thread_handle.thread_id().as_str(),
                    execution.spec.name.as_str(),
                    &execution.display_label,
                    execution.background,
                    SubagentStatus::Running.as_str(),
                    Some(execution.archive.path()),
                )
                .await
        {
            tracing::warn!(
                child_id,
                error = %err,
                "Failed to run subagent start hooks"
            );
        }
    }

    fn execute_child_task(&self, execution: PreparedChildExecution) -> BoxFuture<'_, Result<ChildRunResult>> {
        Box::pin(async move {
            let mut execution = execution;
            // Fail closed on tool exposure: only expose the delegation tools when
            // the child-scoped controller is actually attached. If controller
            // creation failed the child keeps the non-nested toolset even though
            // its config deny-list may omit the delegation tools.
            let nested_tools_enabled = execution.allow_nested_delegation && execution.child_controller.is_some();
            let filtered_tools = filter_child_tools(
                &execution.spec,
                execution.runner.build_universal_tools().await?,
                execution.spec.is_read_only(),
                nested_tools_enabled,
            );
            let allowed_tools = filtered_tools
                .iter()
                .map(|tool| tool.function_name().to_string())
                .collect::<Vec<_>>();
            execution.runner.set_tool_definitions_override(filtered_tools);
            execution.runner.enable_full_auto(&allowed_tools).await;

            let memory_appendix = load_memory_appendix_async(
                &self.config.workspace_root,
                execution.spec.name.as_str(),
                execution.spec.memory,
            )
            .await?;
            let mut task = Task::new(
                format!("subagent-{}", execution.spec.name),
                format!("Subagent {}", execution.spec.name),
                execution.prompt,
            );
            task.instructions = Some(compose_subagent_instructions(&execution.spec, memory_appendix));

            let results = Box::pin(execution.runner.execute_task(&task, &[])).await?;
            let messages = execution.runner.session_messages();
            let transcript_path =
                persist_child_archive(&execution.archive, &messages, execution.spec.name.as_str()).await?;

            Ok(ChildRunResult {
                messages,
                summary: if results.summary.trim().is_empty() {
                    results.outcome.description()
                } else {
                    results.summary.clone()
                },
                outcome: results.outcome,
                transcript_path,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::threads::ThreadManager;
    use crate::tools::registry::PtySessionManager;
    use tempfile::TempDir;

    /// The built-in explorer spec a child record needs.
    fn explorer_spec() -> SubagentSpec {
        diapason_config::builtin_subagents()
            .into_iter()
            .find(|spec| spec.name == "explorer")
            .expect("explorer spec")
    }

    fn test_controller_config(workspace_root: PathBuf) -> SubagentControllerConfig {
        let vt_cfg = DiapasonConfig::default();
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
        let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
        SubagentControllerConfig {
            workspace_root,
            parent_session_id: "parent-session".to_string(),
            parent_model: "test-model".to_string(),
            parent_provider: "openai".to_string(),
            parent_reasoning_effort: ReasoningEffortLevel::Medium,
            api_key: "test-key".to_string(),
            vt_cfg,
            openai_chatgpt_auth: None,
            depth: 0,
            workspace_gated: false,
            exec_sessions,
            output_reader: None,
            pty_manager: pty_sessions.manager().clone(),
            managed_background_runtime: false,
            lock_scope: None,
        }
    }

    fn nested_controller_config(workspace_root: PathBuf, parent_session_id: &str) -> SubagentControllerConfig {
        let mut config = test_controller_config(workspace_root);
        config.parent_session_id = parent_session_id.to_string();
        config.depth = 1;
        // Mirror `child_controller_for`: a nested scope runs under the managed
        // background runtime and therefore never owns background work.
        config.managed_background_runtime = true;
        config
    }

    fn child_record(
        id: &str,
        parent_thread_id: &str,
        spec: &SubagentSpec,
        status: SubagentStatus,
        depth: usize,
    ) -> ChildRecord {
        ChildRecord {
            id: id.to_string(),
            session_id: format!("{id}-session"),
            parent_thread_id: parent_thread_id.to_string(),
            spec: spec.clone(),
            display_label: subagent_display_label(spec),
            status,
            background: false,
            depth,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            completed_at: status.is_terminal().then_some(Utc::now()),
            summary: None,
            error: None,
            archive_metadata: None,
            archive_path: None,
            transcript_path: None,
            effective_config: None,
            stored_messages: Vec::new(),
            last_prompt: Some(format!("prompt-{id}")),
            queued_prompts: VecDeque::new(),
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            thread_handle: None,
            handle: None,
            notify: Arc::new(Notify::new()),
            worktree_path: None,
            child_controller: None,
            closed: false,
        }
    }

    /// A rejected attachment must not publish the runtime of a runner that
    /// never started.
    ///
    /// The record is written under the same state lock that validates the
    /// attachment, so the rejection has to happen first. Recording the archive
    /// path, the effective config and the thread handle before the check
    /// leaves a record that claims a live run even though the run failed to
    /// attach its controller and will not continue.
    #[tokio::test]
    async fn a_rejected_attachment_publishes_no_child_runtime() {
        let temp = TempDir::new().expect("tempdir");
        let workspace_root = temp.path().to_path_buf();
        let controller = SubagentController::new(test_controller_config(workspace_root.clone()))
            .await
            .expect("controller");
        let spec = explorer_spec();
        let existing = Arc::new(
            SubagentController::new(nested_controller_config(workspace_root.clone(), "child-session"))
                .await
                .expect("existing controller"),
        );
        let replacement = Arc::new(
            SubagentController::new(nested_controller_config(workspace_root, "replacement-session"))
                .await
                .expect("replacement controller"),
        );

        let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Running, 1);
        record.child_controller = Some(Arc::clone(&existing));
        controller.state.write().await.children.insert("child".to_string(), record);

        let threads = ThreadManager::new();
        let publication = ChildRuntimePublication {
            archive_metadata: build_subagent_archive_metadata(
                Path::new("/workspace"),
                "test-model",
                "openai",
                "default",
                "medium",
                "parent-session",
                false,
            ),
            archive_path: PathBuf::from("child-archive.json"),
            effective_config: DiapasonConfig::default(),
            thread_handle: threads.start_thread_with_identifier("child", ThreadBootstrap::new(None)),
            child_controller: Some(Arc::clone(&replacement)),
        };
        let mut state = controller.state.write().await;
        let rejection = SubagentController::publish_child_runtime(&mut state, "child", publication);
        let rejection = rejection.expect_err("an occupied record must reject the attachment");
        assert!(format!("{rejection}").contains("already attached"), "{rejection}");

        let record = state.children.get("child").expect("the record survives the rejection");
        assert!(record.archive_metadata.is_none(), "a rejected attachment published metadata");
        assert!(record.archive_path.is_none(), "a rejected attachment published an archive path");
        assert!(record.effective_config.is_none(), "a rejected attachment published a config");
        assert!(record.thread_handle.is_none(), "a rejected attachment published a thread handle");
        let tracked = match record.child_controller.as_ref() {
            Some(attached) => Arc::ptr_eq(attached, &existing),
            None => false,
        };
        assert!(tracked, "the rejected attachment must keep the tracked controller");
        drop(state);

        existing.signal_shutdown().await;
        replacement.signal_shutdown().await;
    }

    /// A cancel that succeeds while a child is queued must stop it starting.
    ///
    /// `launch_child` publishes `Queued` and then starts the run. If the launch
    /// resurrects the record after the cancel — or publishes its handle without
    /// re-checking the record it is about to run — the cancelled task starts
    /// anyway and takes its prompt back out of the queue, so the cancel's
    /// success no longer means the task stopped.
    #[tokio::test]
    async fn a_cancelled_queued_child_is_not_launched() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        let spec = explorer_spec();
        let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Queued, 1);
        record.queued_prompts.push_back("delegated work".to_string());
        controller.state.write().await.children.insert("child".to_string(), record);

        let closed = controller.close(TaskOwner::Workspace, "child").await;
        let closed = closed.expect("cancelling the queued child must succeed");
        assert_eq!(closed.status, SubagentStatus::Closed);

        let launch = controller.launch_child("child").await;
        assert!(launch.is_err(), "a cancelled child must not be launched: {launch:?}");

        let state = controller.state.read().await;
        let record = state.children.get("child").expect("the record survives the refused launch");
        assert_eq!(record.status, SubagentStatus::Closed, "the launch must not resurrect a closed record");
        assert!(record.handle.is_none(), "a refused launch must not publish a task handle");
        assert_eq!(record.queued_prompts.len(), 1, "a refused launch must not consume the prompt");
    }
}

/// Counts each controller tree's runtime captures for tests that must observe
/// the capture rather than a status change that precedes it.
///
/// Keyed by the tree's shared state allocation, pruned when a tree is dropped,
/// so every clone of one controller observes the same count and a test cannot
/// read another test's progress.
#[cfg(test)]
mod runtime_capture_witness {
    use super::*;
    use std::sync::{Mutex, OnceLock, Weak};

    struct CaptureCount {
        state: Weak<RwLock<ControllerState>>,
        captures: u64,
    }

    fn captures() -> &'static Mutex<Vec<CaptureCount>> {
        static CAPTURES: OnceLock<Mutex<Vec<CaptureCount>>> = OnceLock::new();
        CAPTURES.get_or_init(|| Mutex::new(Vec::new()))
    }

    fn with_captures<R>(state: &Arc<RwLock<ControllerState>>, update: impl FnOnce(&mut u64) -> R) -> R {
        let mut registry = captures().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|entry| entry.state.strong_count() > 0);
        if let Some(entry) = registry
            .iter_mut()
            .find(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
        {
            return update(&mut entry.captures);
        }
        let mut count = 0;
        let result = update(&mut count);
        registry.push(CaptureCount { state: Arc::downgrade(state), captures: count });
        result
    }

    /// Records that `state`'s tree published one child runtime capture.
    pub(super) fn observe_capture(state: &Arc<RwLock<ControllerState>>) {
        with_captures(state, |captures| *captures = captures.saturating_add(1));
    }

    /// Returns how many runtime captures `state`'s tree has published.
    pub(super) fn capture_count(state: &Arc<RwLock<ControllerState>>) -> u64 {
        with_captures(state, |captures| *captures)
    }
}
