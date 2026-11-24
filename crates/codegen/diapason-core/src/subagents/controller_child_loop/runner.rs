//! Prepares, executes and publishes one child-agent run.
//!
//! Split out of the supervising loop so the launch, queue and status paths
//! stay readable: everything here runs inside one dequeued child run, from
//! resolving its runtime to publishing the started runner onto its record.

use super::*;

use crate::subagents::controller_spawn_run::ensure_isolation_worktree;
use crate::subagents::controller_tree::ChildRuntimePublication;

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

pub(super) struct ResolvedChildRuntime {
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

impl SubagentController {
    pub(super) fn run_child_once<'a>(
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

    /// Resolves the workspace, model and configuration one child run executes
    /// with.
    ///
    /// Visible to this module's tests so the isolation refusal can be pinned at
    /// the resolution the run actually uses, rather than on a helper alone.
    pub(super) async fn resolve_child_runtime(
        &self,
        context: &ChildExecutionContext,
        request: &ChildRunRequest,
    ) -> Result<ResolvedChildRuntime> {
        // A worktree-isolated child executes and archives against its own root.
        // The record is read once when the run begins, so the checkout can
        // still be reclaimed between that read and this point: a refused
        // launch clears the path, and an operator can remove it. Falling back
        // to the parent workspace would run the task outside the isolation its
        // spec demands, so the run fails closed instead.
        ensure_isolation_worktree(&context.spec, context.worktree_path.as_deref(), context.session_id.as_str())?;
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

    async fn attach_nested_controller(&self, child_id: &str, execution: &mut PreparedChildExecution) {
        let child_controller = self.child_controller_for(child_id, execution).await;
        execution.child_controller = child_controller;
        if let Some(controller) = execution.child_controller.as_ref() {
            controller.set_parent_messages(&execution.bootstrap_messages).await;
            execution.runner.set_subagent_controller(controller.clone());
        }
    }

    async fn record_child_runtime(
        &self,
        child_id: &str,
        execution: &PreparedChildExecution,
    ) -> Result<ThreadRuntimeHandle> {
        let thread_handle = execution.runner.thread_handle();
        self.publish_child_runtime_guarded(
            child_id,
            ChildRuntimePublication {
                archive_metadata: execution.archive_metadata.clone(),
                archive_path: execution.archive.path().to_path_buf(),
                effective_config: execution.child_cfg.clone(),
                thread_handle: thread_handle.clone(),
                child_controller: execution.child_controller.clone(),
            },
            execution.parent_session_id.as_str(),
        )
        .await?;
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
    pub(crate) fn publish_child_runtime(
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
