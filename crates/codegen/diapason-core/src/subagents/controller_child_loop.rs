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
        // A test parks a launch here, after the queued status is published and
        // before the launch re-acquires the state lock, so a close that lands
        // in this window must still refuse the start.
        #[cfg(test)]
        launch_checkpoints::pass(&self.state).await;

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
                let payload = record.build_hook_payload();
                SubagentStopHookContext {
                    parent_session_id: payload.parent_session_id,
                    child_thread_id: payload.child_thread_id,
                    agent_name: payload.agent_name,
                    display_label: payload.display_label,
                    background: payload.background,
                    status: payload.status,
                    transcript_path: payload.transcript_path,
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
}

#[cfg(test)]
pub(in crate::subagents) mod launch_checkpoints;
mod runner;

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

#[cfg(test)]
mod tests;
