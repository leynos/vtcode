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

use crate::config::VTCodeConfig;
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
use vtcode_config::SubagentSpec;
use vtcode_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use super::types::ChildRunRequest;
use vtcode_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::*;

struct ChildExecutionContext {
    spec: SubagentSpec,
    session_id: String,
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
    child_cfg: VTCodeConfig,
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
    child_cfg: VTCodeConfig,
}

struct ChildArchiveRequest<'a> {
    session_id: &'a str,
    workspace: &'a Path,
    config: &'a VTCodeConfig,
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

impl SubagentController {
    pub(super) async fn launch_child(&self, child_id: &str) -> Result<()> {
        // Acquire the lock first to set up the record state, then release it
        // before spawning the task. This avoids the spawned task immediately
        // contending on the write lock.
        {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            record.status = SubagentStatus::Queued;
            record.updated_at = Utc::now();
        }
        self.publish_child_status(child_id).await;

        // Spawn the task after releasing the lock.
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

        // Store the handle in the record.
        let mut state = self.state.write().await;
        if let Some(record) = state.children.get_mut(child_id) {
            record.handle = Some(handle);
        }
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
                if let Err(err) = controller.close(&id).await {
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
            self.attach_child_controller(child_id, &mut execution).await;
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
            parent_session_id: self.parent_session_id.read().await.clone(),
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

    async fn attach_child_controller(&self, child_id: &str, execution: &mut PreparedChildExecution) {
        let child_controller = self.child_controller_for(child_id, execution).await;
        execution.child_controller = child_controller;
        if let Some(controller) = execution.child_controller.as_ref() {
            controller.set_parent_messages(&execution.bootstrap_messages).await;
            execution.runner.set_subagent_controller(controller.clone());
        }
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
            pty_manager: self.config.pty_manager.clone(),
            managed_background_runtime: true,
        };
        match SubagentController::new(nested_config).await {
            Ok(controller) => Some(Arc::new(controller)),
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

    async fn record_child_runtime(
        &self,
        child_id: &str,
        execution: &PreparedChildExecution,
    ) -> Result<ThreadRuntimeHandle> {
        let thread_handle = execution.runner.thread_handle();
        let archive_path = execution.archive.path().to_path_buf();
        {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            record.archive_metadata = Some(execution.archive_metadata.clone());
            record.archive_path = Some(archive_path.clone());
            record.effective_config = Some(execution.child_cfg.clone());
            record.thread_handle = Some(thread_handle.clone());
            if let Some(controller) = execution.child_controller.clone() {
                record.child_controller = Some(controller);
            }
        }
        Ok(thread_handle)
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
