//! Creates and starts subagent runs from controller requests and their configured execution context.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::future::{BoxFuture, select_all};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::core::agent::runner::{AgentRunner, RunnerSettings};
use crate::core::agent::task::Task;
use crate::core::threads::{ThreadBootstrap, ThreadId, ThreadRuntimeHandle, ThreadSnapshot};
use crate::hooks::{LifecycleHookEngine, SessionStartTrigger};
use crate::llm::provider::Message;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::{PtyManager, PtySize};
use crate::utils::session_archive::{SessionArchive, find_session_by_identifier};
use diapason_config::SubagentSpec;
use diapason_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use diapason_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::*;

struct SubagentSpawnOptions {
    fork_context: bool,
    background: bool,
    max_turns: Option<usize>,
    model_override: Option<String>,
    reasoning_override: Option<String>,
}

struct PreparedSubagentSpawn {
    spec: SubagentSpec,
    prompt: String,
    options: SubagentSpawnOptions,
    delegation: PreparedDelegationContext,
}

struct PreparedChildRuntimeConfig {
    effective_config: DiapasonConfig,
    max_turns: Option<usize>,
}

struct QueuedChildRecord {
    id: String,
    record: ChildRecord,
}

enum ChildWaitOutcome {
    Notified,
    Terminal(Box<SubagentStatusEntry>),
    TimedOut,
}

impl SubagentController {
    /// Spawns a new subagent child process from a [`SpawnAgentRequest`].
    pub async fn spawn(&self, request: SpawnAgentRequest) -> Result<SubagentStatusEntry> {
        let mut request = request;
        let delegation = self
            .prepare_delegation_context(
                request.agent_type.clone(),
                &mut request.items,
                &mut request.model,
                "spawn_agent",
            )
            .await?;
        let spec = self.resolve_requested_spec(delegation.requested_agent.as_deref()).await?;
        let prompt = self.prepare_delegation_prompt(
            &spec,
            &delegation,
            &request.message,
            &request.items,
            "spawn_agent",
            "spawning the subagent",
        )?;
        self.spawn_with_spec(PreparedSubagentSpawn {
            spec,
            prompt,
            options: SubagentSpawnOptions {
                fork_context: request.fork_context,
                background: request.background,
                max_turns: request.max_turns,
                model_override: request.model,
                reasoning_override: request.reasoning_effort,
            },
            delegation,
        })
        .await
    }

    /// Spawns a subagent with a custom [`SubagentSpec`] that must be read-only.
    pub fn spawn_custom(
        &self,
        spec: SubagentSpec,
        request: SpawnAgentRequest,
    ) -> BoxFuture<'_, Result<SubagentStatusEntry>> {
        Box::pin(async move {
            if !spec.is_subagent() {
                bail!("custom subagent spawn only supports subagent-capable specs; '{}' is primary-only", spec.name);
            }

            if !spec.is_read_only() {
                bail!(
                    "custom subagent spawn only supports read-only specs; '{}' exposes write-capable behaviour",
                    spec.name
                );
            }

            let mut request = request;
            sanitize_subagent_input_items(&mut request.items);

            let prompt = request_prompt(&request.message, &request.items)
                .or_else(|| spec.initial_prompt.clone())
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("custom subagent spawn requires a task message or items"))?;
            if delegated_task_requires_clarification(&prompt) {
                bail!(
                    "custom subagent task for '{}' is too vague ('{}'). Provide a specific delegated task before spawning the subagent.",
                    spec.name,
                    prompt.trim()
                );
            }

            let mut custom_items = Vec::new();
            let mut custom_model = None;
            let delegation = self
                .prepare_delegation_context(
                    Some(spec.name.clone()),
                    &mut custom_items,
                    &mut custom_model,
                    "spawn_custom",
                )
                .await?;
            self.spawn_with_spec(PreparedSubagentSpawn {
                spec,
                prompt,
                options: SubagentSpawnOptions {
                    fork_context: request.fork_context,
                    background: request.background,
                    max_turns: request.max_turns,
                    model_override: request.model,
                    reasoning_override: request.reasoning_effort,
                },
                delegation,
            })
            .await
        })
    }

    /// Sends additional input to a running or queued subagent.
    pub async fn send_input(&self, request: SendInputRequest) -> Result<SubagentStatusEntry> {
        self.ensure_child_owned_by_invocation(&request.target).await?;
        let prompt = request_prompt(&request.message, &request.items)
            .ok_or_else(|| anyhow!("send_input requires a message or items"))?;

        let maybe_restart = {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(&request.target)
                .ok_or_else(|| anyhow!("Unknown subagent id {}", request.target))?;

            if record.status == SubagentStatus::Closed {
                bail!("Subagent {} is closed", request.target);
            }

            record.updated_at = Utc::now();
            record.last_prompt = Some(prompt.clone());

            if request.interrupt {
                if let Some(handle) = record.handle.take() {
                    handle.abort();
                }
                record.status = SubagentStatus::Queued;
                record.queued_prompts.clear();
                record.queued_prompts.push_back(prompt.clone());
                true
            } else if matches!(record.status, SubagentStatus::Running | SubagentStatus::Queued) {
                record.status = SubagentStatus::Waiting;
                record.queued_prompts.push_back(prompt.clone());
                false
            } else {
                record.status = SubagentStatus::Queued;
                record.queued_prompts.push_back(prompt.clone());
                true
            }
        };

        if maybe_restart {
            self.restart_child(&request.target).await?;
        }

        let status = self.status_for_unchecked(&request.target).await?;
        if !maybe_restart {
            self.publish_subagent_progress(status.clone()).await;
        }
        Ok(status)
    }

    /// Resumes a closed or errored subagent and its descendants by re-queuing
    /// their prompts. Cascades recursively through child-scoped controllers so
    /// grandchildren closed by `close_tree` are resumed too, not merely
    /// un-gated.
    pub async fn resume(&self, target: &str) -> Result<SubagentStatusEntry> {
        self.ensure_child_owned_by_invocation(target).await?;
        self.resume_tree(target).await
    }

    /// Recursively reopens `target` and every descendant across the whole
    /// delegation tree (boxed for async recursion, mirroring [`Self::close_tree`]).
    fn resume_tree(&self, target: &str) -> BoxFuture<'static, Result<SubagentStatusEntry>> {
        let self_owned = self.clone();
        let target_owned = target.to_string();
        Box::pin(async move {
            if self_owned.shutdown_requested.load(Ordering::Relaxed) {
                bail!("Subagent controller is shutting down; cannot resume subagents");
            }
            let subtree_ids = self_owned.collect_spawn_subtree_ids(&target_owned).await?;
            let mut restart_ids = Vec::new();
            for node_id in subtree_ids.iter() {
                if self_owned.reopen_single(node_id.as_str()).await? {
                    restart_ids.push(node_id.clone());
                }
            }
            // Recursively resume each child-scoped controller's descendants,
            // but only for controllers whose owning record is inside the
            // resumed subtree (mirroring close_tree's sibling isolation).
            // Grandchildren live in the child controller's state, so they are
            // reopened through that controller to keep node ownership correct.
            let nested = self_owned.nested_controllers_in_subtree(&subtree_ids).await;
            for (controller, session_id) in nested {
                let ids = controller.spawn_child_ids_for_parent(&session_id).await;
                for id in ids {
                    if let Err(err) = controller.resume_tree(&id).await {
                        tracing::warn!(node_id = id.as_str(), error = %err, "Failed to resume nested subagent subtree");
                    }
                }
            }
            // Re-check shutdown before launching: a concurrent signal_shutdown
            // between reopen and restart would otherwise start a child after
            // shutdown aborted the subtree.
            if self_owned.shutdown_requested.load(Ordering::Relaxed) {
                bail!("Subagent controller is shutting down; cannot resume subagents");
            }
            for restart_id in restart_ids {
                self_owned.restart_child(&restart_id).await?;
            }
            self_owned.status_for_unchecked(&target_owned).await
        })
    }

    /// Closes a subagent and all its descendants, aborting any in-flight work.
    pub async fn close(&self, target: &str) -> Result<SubagentStatusEntry> {
        self.ensure_child_owned_by_invocation(target).await?;
        // `close_tree` walks the full nesting tree (including grandchildren
        // spawned through child-scoped controllers) and closes bottom-up.
        self.close_tree(target).await
    }

    /// Closes `target` and every descendant across the whole delegation tree.
    ///
    /// Unlike [`Self::close`] this is recursive over child-scoped controllers,
    /// so it works for arbitrary `max_depth`. The recursion is boxed to satisfy
    /// Rust's async-fn recursion requirement.
    ///
    /// Ordering prevents a race where a still-running child spawns a new
    /// descendant after the descendant snapshot: each child-scoped controller
    /// is marked as closing (which rejects new spawns via `spawn_with_spec`)
    /// before the owning child's handle is aborted and its subtree is closed.
    fn close_tree(&self, target: &str) -> BoxFuture<'static, Result<SubagentStatusEntry>> {
        let self_owned = self.clone();
        let target_owned = target.to_string();
        Box::pin(async move {
            let subtree_ids = self_owned.collect_spawn_subtree_ids(&target_owned).await?;
            let nested = self_owned.nested_controllers_in_subtree(&subtree_ids).await;
            // Mark every child-scoped controller in the subtree as closing so
            // it rejects new grandchild spawns before we start aborting.
            for (controller, _) in &nested {
                controller.begin_close().await;
            }
            // Close the target's own subtree (deepest first) on this
            // controller. Aborting the owning child's handle first stops it
            // from spawning further descendants.
            let subtree_ids_for_rescan = subtree_ids.clone();
            for node_id in subtree_ids.into_iter().rev() {
                self_owned.close_single(node_id.as_str()).await?;
            }
            // ...then recursively close each child-scoped controller's subtree.
            // Grandchildren live in the child controller's state, not ours, so
            // they are closed through that controller to keep node ownership
            // correct. Only controllers of nodes inside the closed subtree are
            // cascaded, so closing one subagent never kills a sibling's
            // descendants.
            for (controller, session_id) in nested {
                let ids = controller.spawn_child_ids_for_parent(&session_id).await;
                for id in ids {
                    if let Err(err) = controller.close_tree(&id).await {
                        tracing::warn!(node_id = id.as_str(), error = %err, "Failed to close nested subagent subtree");
                    }
                }
            }
            // A child may have attached a freshly created controller between
            // the snapshot above and its handle abort. Re-scan and cascade
            // again so no grandchild outlives the close; the second pass is a
            // no-op in the common case because close_tree is idempotent.
            let late = self_owned.nested_controllers_in_subtree(&subtree_ids_for_rescan).await;
            for (controller, session_id) in late {
                controller.begin_close().await;
                let ids = controller.spawn_child_ids_for_parent(&session_id).await;
                for id in ids {
                    if let Err(err) = controller.close_tree(&id).await {
                        tracing::warn!(node_id = id.as_str(), error = %err, "Failed to close late nested subagent subtree");
                    }
                }
            }
            self_owned.status_for_unchecked(&target_owned).await
        })
    }

    /// Blocks until one of the target subagents reaches a terminal state or the timeout expires.
    pub async fn wait(&self, targets: &[String], timeout_ms: Option<u64>) -> Result<Option<SubagentStatusEntry>> {
        for target in targets {
            self.ensure_child_owned_by_invocation(target).await?;
        }
        if let Some(entry) = self.first_terminal_wait_target(targets).await {
            return Ok(Some(entry));
        }

        let timeout = std::time::Duration::from_millis(
            timeout_ms.unwrap_or_else(|| self.config.vt_cfg.subagents.default_timeout_seconds.saturating_mul(1000)),
        );
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            match self.wait_for_child_change(targets, deadline).await {
                ChildWaitOutcome::Notified => {}
                ChildWaitOutcome::Terminal(entry) => return Ok(Some(*entry)),
                ChildWaitOutcome::TimedOut => return Ok(None),
            }
        }
    }

    async fn first_terminal_wait_target(&self, targets: &[String]) -> Option<SubagentStatusEntry> {
        for target in targets {
            if let Ok(entry) = self.status_for_unchecked(target).await
                && entry.status.is_terminal()
            {
                return Some(entry);
            }
        }
        None
    }

    async fn wait_for_child_change(&self, targets: &[String], deadline: tokio::time::Instant) -> ChildWaitOutcome {
        let notifies = {
            let state = self.state.read().await;
            targets
                .iter()
                .filter_map(|target| state.children.get(target).map(|record| record.notify.clone()))
                .collect::<Vec<_>>()
        };
        if notifies.is_empty() {
            return ChildWaitOutcome::TimedOut;
        }

        // Register notified() futures before rechecking status. Merely
        // constructing a Notified future does not register it; enable() is
        // needed because notify_waiters() does not retain a permit for futures
        // that have not yet been polled.
        let wait_any = select_all(register_notify_waiters(&notifies));
        tokio::pin!(wait_any);

        if let Some(entry) = self.first_terminal_wait_target(targets).await {
            return ChildWaitOutcome::Terminal(Box::new(entry));
        }

        let sleep = tokio::time::sleep_until(deadline);
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut sleep => ChildWaitOutcome::TimedOut,
            _ = &mut wait_any => ChildWaitOutcome::Notified,
        }
    }

    /// Returns the current status of a tracked subagent by its target id.
    pub async fn status_for(&self, target: &str) -> Result<SubagentStatusEntry> {
        self.ensure_child_owned_by_invocation(target).await?;
        self.status_for_unchecked(target).await
    }

    async fn status_for_unchecked(&self, target: &str) -> Result<SubagentStatusEntry> {
        let state = self.state.read().await;
        let record = state
            .children
            .get(target)
            .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
        Ok(record.build_status_entry())
    }

    pub(super) async fn spawn_child_ids_for_parent(&self, parent_thread_id: &str) -> Vec<String> {
        let state = self.state.read().await;
        let mut child_ids = state
            .children
            .values()
            .filter(|record| record.parent_thread_id == parent_thread_id)
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        child_ids.sort();
        child_ids
    }

    pub(super) async fn collect_spawn_subtree_ids(&self, root_thread_id: &str) -> Result<Vec<String>> {
        let mut subtree_ids = Vec::new();
        let mut stack = vec![root_thread_id.to_string()];

        while let Some(thread_id) = stack.pop() {
            subtree_ids.push(thread_id.clone());
            let child_ids = self.spawn_child_ids_for_parent(&thread_id).await;
            for child_id in child_ids.into_iter().rev() {
                stack.push(child_id);
            }
        }

        Ok(subtree_ids)
    }

    /// Collects the child-scoped controllers owned by records inside `subtree_ids`,
    /// paired with each owning record's session id. Shared by `resume_tree` and
    /// `close_tree` so subtree isolation cannot diverge between them.
    async fn nested_controllers_in_subtree(&self, subtree_ids: &[String]) -> Vec<(Arc<SubagentController>, String)> {
        let subtree_set = subtree_ids.iter().collect::<std::collections::HashSet<_>>();
        let state = self.state.read().await;
        state
            .children
            .iter()
            .filter(|(id, _)| subtree_set.contains(id))
            .filter_map(|(_, record)| {
                record
                    .child_controller
                    .clone()
                    .map(|controller| (controller, record.session_id.clone()))
            })
            .collect()
    }

    pub(super) async fn reopen_single(&self, target: &str) -> Result<bool> {
        let child_controller = {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(target)
                .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
            if matches!(record.status, SubagentStatus::Running | SubagentStatus::Queued) {
                return Ok(false);
            }
            let prompt = record
                .last_prompt
                .clone()
                .unwrap_or_else(|| "Continue the delegated task from the existing context.".to_string());
            record.status = SubagentStatus::Queued;
            record.updated_at = Utc::now();
            record.completed_at = None;
            record.error = None;
            record.summary = None;
            record.queued_prompts.push_back(prompt);
            record.child_controller.clone()
        };
        // Reopening a subtree reverses the transient `begin_close` on any
        // child-scoped controller so a resumed child can delegate again (and
        // its controller resumes saving background state).
        if let Some(controller) = child_controller {
            controller.end_close().await;
        }
        Ok(true)
    }

    async fn close_single(&self, target: &str) -> Result<SubagentStatusEntry> {
        let mut state = self.state.write().await;
        let record = state
            .children
            .get_mut(target)
            .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
        // Cancellation must not rewrite a settled outcome. `Completed` and
        // `Failed` children already stopped running, and a transport projects
        // `Closed` as a failure, so closing them again would turn a
        // successful task into a visibly failed one and overwrite its
        // completion timestamp.
        if record.status.is_terminal() {
            return Ok(record.build_status_entry());
        }
        if let Some(handle) = record.handle.take() {
            handle.abort();
        }
        record.status = SubagentStatus::Closed;
        record.updated_at = Utc::now();
        record.completed_at = Some(Utc::now());
        record.notify.notify_waiters();
        let entry = record.build_status_entry();
        drop(state);
        self.publish_subagent_progress(entry.clone()).await;
        Ok(entry)
    }

    /// Marks this controller as closing so [`Self::spawn`] rejects new spawns.
    ///
    /// Used before aborting a subtree so a still-running child cannot spawn a
    /// descendant between the descendant snapshot and the handle abort. Unlike
    /// [`Self::signal_shutdown`] this uses the transient `closing` flag, which
    /// is cleared again when the subtree is reopened, so a resumed child can
    /// delegate and its controller keeps saving background state.
    pub(super) async fn begin_close(&self) {
        self.closing.store(true, Ordering::Relaxed);
    }

    /// Clears the transient close-in-progress flag. Called when a closed
    /// subtree is reopened so its child-scoped controller can delegate again.
    pub(super) async fn end_close(&self) {
        self.closing.store(false, Ordering::Relaxed);
    }

    /// Signal that the program is shutting down. Subsequent calls to
    /// `save_background_state` will be skipped. All running child handles
    /// are aborted so subagent tasks do not outlive the parent session.
    pub async fn signal_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        let (nested, entries) = self.close_children_for_shutdown().await;
        for entry in entries {
            self.publish_subagent_progress(entry).await;
        }
        self.shutdown_nested_controllers(nested).await;
    }

    async fn close_children_for_shutdown(&self) -> (Vec<(Arc<SubagentController>, String)>, Vec<SubagentStatusEntry>) {
        let mut state = self.state.write().await;
        let mut nested = Vec::new();
        for record in state.children.values_mut() {
            if let Some(handle) = record.handle.take() {
                handle.abort();
            }
            record.status = SubagentStatus::Closed;
            record.completed_at = Some(Utc::now());
            record.notify.notify_waiters();
            if let Some(controller) = record.child_controller.clone() {
                nested.push((controller, record.session_id.clone()));
            }
        }
        let entries = state.children.values().map(ChildRecord::build_status_entry).collect::<Vec<_>>();
        (nested, entries)
    }

    async fn shutdown_nested_controllers(&self, nested: Vec<(Arc<SubagentController>, String)>) {
        // Mark every child-scoped controller as permanently shut down (so its
        // background state is not saved) and as closing (so a race cannot let
        // a fresh grandchild outlive shutdown), then cascade.
        for (controller, _) in &nested {
            controller.shutdown_requested.store(true, Ordering::Relaxed);
            controller.begin_close().await;
        }
        // Cascade shutdown to child-scoped controllers so grandchildren tasks
        // are aborted too; otherwise their tokio tasks keep running detached.
        for (controller, session_id) in nested {
            let ids = controller.spawn_child_ids_for_parent(&session_id).await;
            for id in ids {
                if let Err(err) = controller.close_tree(&id).await {
                    tracing::warn!(node_id = id.as_str(), error = %err, "Failed to close nested subagent subtree during shutdown");
                }
            }
        }
    }

    pub(super) async fn save_background_state(&self) -> Result<()> {
        if self.shutdown_requested.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self.config.managed_background_runtime {
            // Nested scopes never own background records. Persisting their
            // always-empty view would erase the ledger owned by the root
            // controller.
            return Ok(());
        }
        // Take the persistence lock before snapshotting in-memory state.
        // Reading first lets two concurrent saves snapshot v1 and v2, then
        // write v2 before v1, leaving the older snapshot as the file. The lock
        // must cover the read so the last write is always the newest state.
        let guard = background_state_persistence_lock().lock().await;
        // The in-process mutex cannot serialise another Diapason process that
        // shares this workspace. Hold the ledger's inter-process lock across
        // the reload, merge and write so two processes cannot each read the
        // old ledger and then have the later rename discard the other's
        // record.
        let file_guard = lock_background_state(&self.config.workspace_root).await?;
        let records = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .cloned()
                .map(BackgroundRecord::into_persisted)
                .collect()
        };
        let workspace_root = self.config.workspace_root.clone();
        let owner_session_id = self.background_owner_session_id.clone();
        // Both guards move into the persistence task. Cancelling this caller
        // only detaches that task, so neither lock is released until the
        // reload, merge, write and rename have finished; dropping them here
        // instead would let a second writer start from the same stale ledger
        // while this write was still in flight.
        tokio::spawn(async move {
            let _persistence_guard = guard;
            let _ledger_guard = file_guard;
            persist_background_records(&workspace_root, owner_session_id.as_deref(), records).await
        })
        .await
        .context("Failed to join the background state persistence task")?
    }

    pub(super) async fn find_spec(&self, candidate: &str) -> Option<SubagentSpec> {
        self.state
            .read()
            .await
            .discovered
            .effective
            .iter()
            .find(|spec| spec.is_subagent() && spec.matches_name(candidate))
            .cloned()
    }

    pub(super) async fn resolve_requested_spec(&self, requested: Option<&str>) -> Result<SubagentSpec> {
        let requested = requested.unwrap_or("default");
        self.find_spec(requested)
            .await
            .ok_or_else(|| anyhow!("Unknown subagent type {requested}"))
    }

    pub(super) async fn prepare_delegation_context(
        &self,
        requested_agent: Option<String>,
        items: &mut Vec<SubagentInputItem>,
        model: &mut Option<String>,
        tool_name: &'static str,
    ) -> Result<PreparedDelegationContext> {
        let state = self.state.read().await;
        sanitize_subagent_input_items(items);
        let invocation = current_subagent_invocation_context();
        let current_input = invocation
            .as_ref()
            .map_or(state.turn_hints.current_input.as_str(), |context| context.current_input.as_str());
        let explicit_mentions = invocation.as_ref().map_or_else(
            || state.turn_hints.explicit_mentions.clone(),
            |_| extract_explicit_agent_mentions(current_input, state.discovered.effective.as_slice()),
        );
        let explicit_request = invocation.as_ref().map_or(state.turn_hints.explicit_request, |_| {
            contains_explicit_delegation_request(current_input, explicit_mentions.as_slice())
        });
        *model = normalize_requested_model_override(model.take(), current_input);
        let requested_agent = if let Some(agent_type) = requested_agent {
            Some(agent_type)
        } else {
            match explicit_mentions.as_slice() {
                [] => None,
                [single] => Some(single.clone()),
                mentions => {
                    bail!(
                        "{} omitted agent_type, but the user explicitly selected multiple agents: {}. Specify agent_type explicitly.",
                        tool_name,
                        mentions.join(", ")
                    );
                }
            }
        };
        let parent_messages = invocation
            .as_ref()
            .map(|context| context.parent_messages.clone())
            .unwrap_or_else(|| state.parent_messages.clone());
        drop(state);
        let parent_session_id = match invocation {
            Some(context) => context.parent_session_id,
            None => self.parent_session_id.read().await.clone(),
        };
        Ok(PreparedDelegationContext {
            requested_agent,
            explicit_mentions,
            explicit_request,
            parent_session_id,
            parent_messages,
        })
    }

    pub(super) fn prepare_delegation_prompt(
        &self,
        spec: &SubagentSpec,
        delegation: &PreparedDelegationContext,
        message: &Option<String>,
        items: &[SubagentInputItem],
        tool_name: &'static str,
        launch_phrase: &'static str,
    ) -> Result<String> {
        if let Some(explicit) = delegation.explicit_mentions.first()
            && delegation.explicit_mentions.len() == 1
            && !spec.matches_name(explicit)
        {
            bail!(
                "{} requested agent_type '{}', but the user explicitly selected '{}'. Use the selected agent or ask the user to clarify.",
                tool_name,
                spec.name,
                explicit
            );
        }
        if !spec.is_read_only() && !delegation.explicit_request && delegation.requested_agent.is_none() {
            bail!(
                "{} cannot launch write-capable agent '{}' without an explicit delegation signal from the current user turn. Ask the user to mention the agent, say 'delegate'/'spawn', or request parallel work.",
                tool_name,
                spec.name
            );
        }
        if spec.is_read_only() && !self.config.vt_cfg.subagents.auto_delegate_read_only && !delegation.explicit_request
        {
            bail!(
                "{} cannot proactively launch read-only agent '{}' because `subagents.auto_delegate_read_only` is disabled and the current user turn did not explicitly request delegation.",
                tool_name,
                spec.name
            );
        }
        let prompt = request_prompt(message, items)
            .or_else(|| spec.initial_prompt.clone())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("{tool_name} requires a task message or items"))?;
        if delegated_task_requires_clarification(&prompt) {
            bail!(
                "{} task for '{}' is too vague ('{}'). Ask the user for a specific delegated task before {}.",
                tool_name,
                spec.name,
                prompt.trim(),
                launch_phrase
            );
        }
        Ok(prompt)
    }

    fn spawn_with_spec(&self, launch: PreparedSubagentSpawn) -> BoxFuture<'_, Result<SubagentStatusEntry>> {
        Box::pin(async move {
            // Admit and validate before creating a worktree: every rejection
            // that can happen here would otherwise leave an unreferenced
            // checkout behind.
            self.ensure_spawn_allowed(&launch.spec)?;
            self.ensure_spawn_capacity().await?;
            let runtime = self.prepare_spawn_runtime(&launch)?;
            let worktree_path = self.create_spawn_worktree(&launch.spec).await?;
            let worktree_path_for_cleanup = worktree_path.clone();
            let child = self.prepare_queued_child(launch, worktree_path, runtime);
            // The stored id is authoritative: insertion may have to
            // disambiguate it against a child registered in the same
            // millisecond, so launching the pre-insertion id would address the
            // wrong record, or none at all.
            let id = match Box::pin(self.insert_queued_child(child)).await {
                Ok(id) => id,
                Err(error) => {
                    // The record never became visible, so nothing owns the
                    // worktree; reclaim it rather than leaking the checkout.
                    if let Some(worktree_path) = worktree_path_for_cleanup.as_deref() {
                        self.remove_unused_spawn_worktree(worktree_path).await;
                    }
                    return Err(error);
                }
            };
            self.launch_child(&id).await?;
            self.status_for(&id).await
        })
    }

    fn ensure_spawn_allowed(&self, spec: &SubagentSpec) -> Result<()> {
        if !self.config.vt_cfg.subagents.enabled {
            bail!("Subagents are disabled by configuration");
        }
        if self.shutdown_requested.load(Ordering::Relaxed) || self.closing.load(Ordering::Relaxed) {
            bail!("Subagent controller is shutting down; cannot spawn new subagents");
        }
        if self.config.depth.saturating_add(1) > self.config.vt_cfg.subagents.max_depth {
            bail!("Subagent depth limit reached (max_depth={})", self.config.vt_cfg.subagents.max_depth);
        }
        if self.config.depth > 0 && spec.isolation == Some(diapason_config::IsolationMode::Worktree) {
            bail!(
                "Subagent '{}' requests isolation=worktree, but nested worktree isolation is not supported \
                 (child-scoped controllers operate inside the parent's worktree). Use isolation=worktree only \
                 at the root delegation level.",
                spec.name
            );
        }
        Ok(())
    }

    async fn create_spawn_worktree(&self, spec: &SubagentSpec) -> Result<Option<PathBuf>> {
        // Create a worktree for isolation if requested.
        if spec.isolation == Some(diapason_config::IsolationMode::Worktree) {
            let workspace_root = self.config.workspace_root.clone();
            let worktree_name =
                format!("{}-{}", sanitize_component(spec.name.as_str()), Utc::now().format("%Y%m%dT%H%M%S"));
            let worktree_name_for_error = worktree_name.clone();
            let worktree_result = tokio::task::spawn_blocking(move || {
                crate::git::WorktreeManager::new(workspace_root).create(&worktree_name)
            })
            .await
            .context("Worktree creation task panicked")?;
            Ok(Some(worktree_result.with_context(|| {
                format!("Failed to create worktree for subagent '{}'", worktree_name_for_error)
            })?))
        } else {
            Ok(None)
        }
    }

    /// Best-effort removal of a worktree whose child record was never registered.
    ///
    /// Cleanup failures are logged rather than propagated so they cannot mask
    /// the registration error that caused the rollback.
    async fn remove_unused_spawn_worktree(&self, worktree_path: &std::path::Path) {
        let Some(name) = worktree_path.file_name().and_then(|name| name.to_str()).map(str::to_owned) else {
            tracing::warn!(
                path = %worktree_path.display(),
                "Cannot reclaim subagent worktree with a non-UTF-8 name"
            );
            return;
        };
        let workspace_root = self.config.workspace_root.clone();
        let removal =
            tokio::task::spawn_blocking(move || crate::git::WorktreeManager::new(workspace_root).remove(&name)).await;
        match removal {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                path = %worktree_path.display(),
                error = %error,
                "Failed to reclaim subagent worktree after registration failed"
            ),
            Err(error) => tracing::warn!(
                path = %worktree_path.display(),
                error = %error,
                "Subagent worktree cleanup task panicked"
            ),
        }
    }

    async fn ensure_spawn_capacity(&self) -> Result<()> {
        let active_count = {
            let state = self.state.read().await;
            state
                .children
                .values()
                .filter(|record| {
                    matches!(record.status, SubagentStatus::Queued | SubagentStatus::Running | SubagentStatus::Waiting)
                })
                .count()
        };
        let effective_max_concurrent = self.config.vt_cfg.subagents.max_concurrent.min(SUBAGENT_HARD_CONCURRENCY_LIMIT);
        if active_count >= effective_max_concurrent {
            bail!("Subagent concurrency limit reached (max_concurrent={effective_max_concurrent})");
        }
        Ok(())
    }

    fn prepare_spawn_runtime(&self, launch: &PreparedSubagentSpawn) -> Result<PreparedChildRuntimeConfig> {
        let is_background_child = launch.options.background;
        let child_max_turns = normalize_background_child_max_turns(
            launch.options.max_turns.or(launch.spec.max_turns),
            is_background_child,
        );
        let (_, _, effective_config) = prepare_child_runtime_config(
            &self.config.vt_cfg,
            &launch.spec,
            self.config.parent_model.as_str(),
            self.config.parent_provider.as_str(),
            self.config.parent_reasoning_effort,
            child_max_turns,
            launch.options.model_override.as_deref(),
            launch.options.reasoning_override.as_deref(),
            !launch.spec.is_read_only()
                && self.config.depth.saturating_add(2) <= self.config.vt_cfg.subagents.max_depth,
            resolve_effective_subagent_model,
        )?;
        Ok(PreparedChildRuntimeConfig { effective_config, max_turns: child_max_turns })
    }

    fn prepare_queued_child(
        &self,
        launch: PreparedSubagentSpawn,
        worktree_path: Option<PathBuf>,
        runtime: PreparedChildRuntimeConfig,
    ) -> QueuedChildRecord {
        let id = format!(
            "agent-{}-{}",
            sanitize_component(launch.spec.name.as_str()),
            Utc::now().format("%Y%m%dT%H%M%S%3fZ")
        );
        let parent_session_id = launch.delegation.parent_session_id.clone();
        let session_id =
            format!("{}-{}", sanitize_component(parent_session_id.as_str()), sanitize_component(id.as_str()));
        let display_label = subagent_display_label(&launch.spec);
        let notify = Arc::new(Notify::new());
        let initial_messages = if launch.options.fork_context {
            launch.delegation.parent_messages
        } else {
            Vec::new()
        };
        let record = ChildRecord {
            id: id.clone(),
            session_id,
            parent_thread_id: parent_session_id,
            spec: launch.spec,
            display_label,
            status: SubagentStatus::Queued,
            background: launch.options.background,
            depth: self.config.depth.saturating_add(1),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            completed_at: None,
            summary: None,
            error: None,
            archive_metadata: None,
            archive_path: None,
            transcript_path: None,
            effective_config: Some(runtime.effective_config),
            stored_messages: initial_messages,
            last_prompt: Some(launch.prompt.clone()),
            queued_prompts: VecDeque::from([launch.prompt]),
            max_turns: runtime.max_turns,
            model_override: launch.options.model_override,
            reasoning_override: launch.options.reasoning_override,
            thread_handle: None,
            handle: None,
            notify,
            worktree_path,
            child_controller: None,
        };
        QueuedChildRecord { id, record }
    }

    /// Inserts a prepared child and returns the id it was stored under.
    ///
    /// Generated ids carry millisecond precision, so two spawns of the same
    /// kind in one millisecond propose the same id, and the colliding pair need
    /// not even belong to the same controller: a session exposes its whole
    /// controller tree and routes an id to the first match. `BTreeMap::insert`
    /// would replace the earlier record, silently dropping a subagent that has
    /// already been admitted and may already own a worktree. Allocating the
    /// unique id inside the write lock, against the tree-wide scope, is what
    /// makes the check race-free, so the record's id and session id are both
    /// rewritten before insertion.
    async fn insert_queued_child(&self, mut child: QueuedChildRecord) -> Result<String> {
        let mut state = self.state.write().await;
        // Re-check the close gates while the write lock is held. The earlier
        // check can race with a concurrent `close_tree` that snapshots and
        // closes the subtree between this spawn's admission and its record
        // insertion; checking after acquisition closes that window.
        if self.shutdown_requested.load(Ordering::Relaxed) || self.closing.load(Ordering::Relaxed) {
            bail!("Subagent controller is shutting down; cannot spawn new subagents");
        }
        let unique_id = self
            .child_id_scope
            .allocate(|id| state.children.contains_key(id), child.id.as_str());
        if unique_id != child.id {
            child.record.session_id = format!(
                "{}-{}",
                sanitize_component(child.record.parent_thread_id.as_str()),
                sanitize_component(unique_id.as_str())
            );
            child.record.id = unique_id.clone();
            child.id = unique_id;
        }
        let id = child.id.clone();
        state.children.insert(child.id, child.record);
        Ok(id)
    }

    /// Inserts a prepared record under a caller-chosen candidate id.
    ///
    /// Test seam only: a real spawn derives its id from the agent kind and a
    /// millisecond clock, so two controllers can only be forced to propose the
    /// same id by supplying the candidate directly. Everything else about the
    /// insertion — the close-gate recheck, the tree-wide allocation, and the
    /// returned id — is the production path.
    #[cfg(test)]
    pub(super) async fn insert_child_with_candidate_id(
        &self,
        candidate_id: &str,
        record: ChildRecord,
    ) -> Result<String> {
        // The insertion future carries the whole record, and a test future that
        // large trips `clippy::large_futures`; boxing it here keeps the seam's
        // own future small without changing the production path.
        Box::pin(self.insert_queued_child(QueuedChildRecord { id: candidate_id.to_owned(), record })).await
    }

    async fn restart_child(&self, target: &str) -> Result<()> {
        let has_queued_input = {
            let mut state = self.state.write().await;
            let record = state
                .children
                .get_mut(target)
                .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
            if record.queued_prompts.is_empty()
                && let Some(prompt) = record.last_prompt.clone()
            {
                record.queued_prompts.push_back(prompt);
            }
            !record.queued_prompts.is_empty()
        };
        if !has_queued_input {
            bail!("Subagent {target} has no queued input");
        }
        self.launch_child(target).await
    }
}

fn register_notify_waiters(notifies: &[Arc<Notify>]) -> Vec<Pin<Box<tokio::sync::futures::Notified<'_>>>> {
    let mut waiters = notifies.iter().map(|notify| Box::pin(notify.notified())).collect::<Vec<_>>();
    for waiter in &mut waiters {
        waiter.as_mut().enable();
    }
    waiters
}

static BACKGROUND_STATE_PERSISTENCE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn background_state_persistence_lock() -> &'static Mutex<()> {
    BACKGROUND_STATE_PERSISTENCE_LOCK.get_or_init(|| Mutex::new(()))
}

/// Reloads the shared ledger, merges one controller's snapshot and writes it.
///
/// The caller holds both persistence locks; this function owns the reload,
/// merge and write so they complete as one critical section.
async fn persist_background_records(
    workspace_root: &std::path::Path,
    owner_session_id: Option<&str>,
    records: Vec<PersistedBackgroundRecord>,
) -> Result<()> {
    if let Some(owner_session_id) = owner_session_id {
        let mut persisted_records = load_background_state(workspace_root).await?.records;
        persisted_records.retain(|record| record.owner_session_id.as_deref() != Some(owner_session_id));
        persisted_records.extend(records);
        persist_background_state(workspace_root, persisted_records).await
    } else {
        // An unscoped controller shares this ledger with the owner-scoped
        // controllers of other sessions and processes, and it loads their
        // records so it can list and route them. Replacing the file with its
        // own snapshot would delete a record another owner created after this
        // controller loaded, or restore that owner's record from the stale
        // copy it holds.
        let persisted_records = load_background_state(workspace_root).await?.records;
        persist_background_state(workspace_root, merge_unscoped_background_records(persisted_records, records)).await
    }
}

/// Returns `candidate`, or the first `-<n>` suffix of it that `is_taken` does
/// not already hold.
///
/// A generated id is `<kind>-<millisecond timestamp>`, so the only collision
/// it can suffer is a second spawn of the same kind in the same millisecond.
/// Suffixing keeps both records addressable instead of letting the second
/// insert replace the first. Ids reach this function only from the generator
/// in `prepare_queued_child`, so no caller-supplied naming scheme gains an
/// unexpected suffix.
fn unique_child_id(is_taken: impl Fn(&str) -> bool, candidate: &str) -> String {
    if !is_taken(candidate) {
        return candidate.to_owned();
    }
    let mut suffix = 2_u32;
    loop {
        let id = format!("{candidate}-{suffix}");
        if !is_taken(&id) {
            return id;
        }
        suffix = suffix.saturating_add(1);
    }
}

/// The child task ids allocated anywhere in one controller tree.
///
/// A controller only holds its own children, so checking that map alone cannot
/// see an id a sibling controller already allocated. The owning session
/// exposes the whole tree at once and routes a task id to the first controller
/// that matches, so a duplicate id would list one task twice and send an
/// output read or a cancel to whichever controller happened to come first.
/// The root controller owns one scope and every descendant inherits the same
/// `Arc`; ids are recorded for the lifetime of the tree.
#[derive(Default)]
pub(super) struct ChildIdScope {
    taken: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

impl ChildIdScope {
    /// Allocates `candidate` or its first free suffix, and records the result.
    ///
    /// The caller supplies the controller-local collision check; this scope
    /// adds the tree-wide one and remembers the allocated id so a later
    /// allocation in any controller of the tree cannot repeat it.
    pub(super) fn allocate(&self, is_taken: impl Fn(&str) -> bool, candidate: &str) -> String {
        let mut taken = self.taken.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let id = unique_child_id(|id| taken.contains(id) || is_taken(id), candidate);
        taken.insert(id.clone());
        id
    }
}

/// Merges an unscoped controller's snapshot into the persisted ledger.
///
/// The unscoped controller is not the owner of the owner-scoped records it
/// loads, so it must not replace them with its own copy. Records it does not
/// track are kept exactly as another process last wrote them, and a record
/// both sides hold keeps the copy with the newest `updated_at`: an untouched
/// snapshot equals the copy this controller loaded, while a local refresh or
/// cancellation is newer than it.
fn merge_unscoped_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
) -> Vec<PersistedBackgroundRecord> {
    // Key on the canonical id: a ledger written by an older build can hold a
    // legacy id for the same owner-scoped record, and the load path
    // canonicalizes in memory. Keying on the raw id would let one record
    // appear twice, or let two owners' records collapse into one entry.
    let mut merged: BTreeMap<String, PersistedBackgroundRecord> = persisted
        .into_iter()
        .map(|mut record| {
            record.canonicalize_id();
            (record.id.clone(), record)
        })
        .collect();
    for mut record in snapshot {
        record.canonicalize_id();
        match merged.get_mut(&record.id) {
            Some(current) if record.is_at_least_as_new_as(current) => *current = record,
            Some(_) => {}
            None => {
                merged.insert(record.id.clone(), record);
            }
        }
    }
    merged.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// Generated ids carry millisecond precision, so two spawns of the same
    /// kind in one millisecond propose the same id. The second must receive a
    /// distinct id: `BTreeMap::insert` on the shared key would replace the
    /// first child's record and silently lose a subagent that had already been
    /// admitted.
    #[test]
    fn a_colliding_child_id_is_suffixed_instead_of_replacing_the_record() {
        let mut taken: BTreeSet<String> = BTreeSet::new();

        let first = unique_child_id(|id| taken.contains(id), "agent-explorer-20261113T060819731Z");
        taken.insert(first.clone());
        let second = unique_child_id(|id| taken.contains(id), "agent-explorer-20261113T060819731Z");
        taken.insert(second.clone());
        let third = unique_child_id(|id| taken.contains(id), "agent-explorer-20261113T060819731Z");

        assert_eq!(
            [first.as_str(), second.as_str(), third.as_str()],
            [
                "agent-explorer-20261113T060819731Z",
                "agent-explorer-20261113T060819731Z-2",
                "agent-explorer-20261113T060819731Z-3",
            ],
            "each collision must take the next free suffix"
        );
        assert!(!taken.contains(&third), "the third id must still be free");
    }

    #[tokio::test]
    async fn registered_waiter_observes_notify_waiters_before_first_wait_poll() {
        let notify = Arc::new(Notify::new());
        let notifies = [Arc::clone(&notify)];
        let waiters = register_notify_waiters(&notifies);

        // notify_waiters() is lossy for futures that have not registered.
        // Deliver it after enable() but before select_all first polls a waiter.
        notify.notify_waiters();

        let outcome = tokio::time::timeout(Duration::from_millis(50), select_all(waiters)).await;
        assert!(outcome.is_ok(), "registered waiter should retain the wakeup");
    }

    /// The owner/agent shapes whose records must stay distinct after a merge.
    ///
    /// Entries 0 and 1 share an agent name but not an owner, and entry 2 is the
    /// same agent with no owner at all. A legacy (unscoped) id therefore
    /// collides across them, which is exactly what canonicalization must
    /// separate rather than collapse.
    const MERGE_SHAPES: [(&str, Option<&str>); 4] = [
        ("worker", Some("session-a")),
        ("worker", Some("session-b")),
        ("worker", None),
        ("reviewer", Some("session-a")),
    ];

    /// One generated merge input.
    #[derive(Debug, Clone, Copy)]
    struct MergeRecord {
        /// Index into [`MERGE_SHAPES`], selecting the agent and owner.
        shape: usize,
        /// Seconds past the fixture epoch at which the record was updated.
        updated_offset: u8,
        /// Whether the record still carries the pre-canonicalization id.
        legacy_id: bool,
    }

    /// Generates a record with an arbitrary shape, freshness and id form.
    fn merge_record() -> impl Strategy<Value = MergeRecord> {
        (0usize..MERGE_SHAPES.len(), 0u8..3u8, any::<bool>())
            .prop_map(|(shape, updated_offset, legacy_id)| MergeRecord { shape, updated_offset, legacy_id })
    }

    /// The canonical key, recomputed from the agent and owner alone.
    ///
    /// The generated names contain only characters that survive sanitization,
    /// so this is the documented id format rather than a call back into the
    /// production helper the merge uses.
    fn merge_canonical_id(shape: usize) -> String {
        let (agent, owner) = MERGE_SHAPES[shape];
        match owner {
            Some(owner) => format!("background-{agent}@{owner}"),
            None => format!("background-{agent}"),
        }
    }

    /// Renders one persisted record, tagging it so its provenance survives.
    fn merge_record_json(record: &MergeRecord, tag: &str) -> serde_json::Value {
        let (agent, owner) = MERGE_SHAPES[record.shape];
        let id = if record.legacy_id {
            format!("background-{agent}")
        } else {
            merge_canonical_id(record.shape)
        };
        serde_json::json!({
            "id": id,
            "owner_session_id": owner,
            "agent_name": agent,
            "display_label": agent,
            "description": "merge fixture",
            "source": "builtin",
            "color": null,
            "session_id": "child-session",
            "exec_session_id": "exec-session",
            "desired_enabled": true,
            "status": "running",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": format!("2026-08-28T00:00:0{}Z", record.updated_offset),
            "started_at": null,
            "ended_at": null,
            "pid": null,
            "prompt": "work",
            "summary": tag,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        })
    }

    /// Deserializes generated records and records each tag's own timestamp.
    ///
    /// The ledger keeps the tags so the property can compare the winning
    /// record's timestamp with the input it came from.
    fn merge_input(
        records: &[MergeRecord],
        prefix: &str,
        updated_by_tag: &mut BTreeMap<String, i64>,
    ) -> Vec<PersistedBackgroundRecord> {
        records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let tag = format!("{prefix}{index}");
                let value = merge_record_json(record, &tag);
                updated_by_tag.insert(tag, merge_timestamp_millis(&value["updated_at"]));
                serde_json::from_value(value).expect("generated persisted record")
            })
            .collect()
    }

    /// Parses a serialized timestamp so comparisons do not depend on format.
    fn merge_timestamp_millis(value: &serde_json::Value) -> i64 {
        chrono::DateTime::parse_from_rfc3339(value.as_str().expect("serialized timestamp"))
            .expect("parse serialized timestamp")
            .timestamp_millis()
    }

    proptest! {
        /// The unscoped merge is a per-key join against an independent model.
        ///
        /// The model canonicalizes each generated id from its agent and owner,
        /// keeps the last copy a ledger repeats, prefers the snapshot on a tie,
        /// and keeps every record only one side holds. It never calls the
        /// production id or freshness helpers, so a change to either contract
        /// shows up as a disagreement rather than being mirrored by the test.
        #[test]
        fn unscoped_merge_joins_records_by_canonical_key(
            persisted in prop::collection::vec(merge_record(), 0..=5),
            snapshot in prop::collection::vec(merge_record(), 0..=5),
        ) {
            let mut updated_by_tag = BTreeMap::new();
            let persisted_records = merge_input(&persisted, "p", &mut updated_by_tag);
            let snapshot_records = merge_input(&snapshot, "s", &mut updated_by_tag);

            let mut expected: BTreeMap<String, (u8, String)> = BTreeMap::new();
            for (index, record) in persisted.iter().enumerate() {
                expected.insert(merge_canonical_id(record.shape), (record.updated_offset, format!("p{index}")));
            }
            for (index, record) in snapshot.iter().enumerate() {
                let key = merge_canonical_id(record.shape);
                let tag = format!("s{index}");
                match expected.get_mut(&key) {
                    Some(current) if record.updated_offset >= current.0 => *current = (record.updated_offset, tag),
                    Some(_) => {}
                    None => {
                        expected.insert(key, (record.updated_offset, tag));
                    }
                }
            }

            let merged = merge_unscoped_background_records(persisted_records, snapshot_records);
            let observed = merged
                .iter()
                .map(|record| {
                    let value = serde_json::to_value(record).expect("serialize merged record");
                    (
                        value["id"].as_str().expect("merged id").to_owned(),
                        value["summary"].as_str().expect("provenance tag").to_owned(),
                        merge_timestamp_millis(&value["updated_at"]),
                    )
                })
                .collect::<Vec<_>>();

            prop_assert_eq!(observed.len(), expected.len());
            for (entry, (key, (_, tag))) in observed.iter().zip(expected.iter()) {
                prop_assert_eq!(&entry.0, key, "records are canonicalized and ordered by key");
                prop_assert_eq!(&entry.1, tag, "the newest copy wins and a tie keeps the snapshot");
                prop_assert_eq!(Some(&entry.2), updated_by_tag.get(tag), "the winner keeps its own timestamp");
            }
        }
    }
}
