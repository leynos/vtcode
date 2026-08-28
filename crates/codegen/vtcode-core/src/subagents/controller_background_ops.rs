//! Manages background subagent work, including task completion, cancellation, and archived results.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::future::select_all;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, RwLock};

use crate::config::VTCodeConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::core::agent::runner::{AgentRunner, RunnerSettings};
use crate::core::agent::task::Task;
use crate::core::threads::{ThreadBootstrap, ThreadId, ThreadRuntimeHandle, ThreadSnapshot};
use crate::hooks::{LifecycleHookEngine, SessionStartTrigger};
use crate::llm::provider::Message;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::{PtyManager, PtySize};
use crate::utils::session_archive::{SessionArchive, find_session_by_identifier};
use vtcode_config::SubagentSpec;
use vtcode_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use vtcode_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

struct CapturedSubagentThread {
    id: String,
    session_id: String,
    parent_thread_id: String,
    agent_name: String,
    display_label: String,
    status: SubagentStatus,
    background: bool,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    archive_path: Option<PathBuf>,
    transcript_path: Option<PathBuf>,
    effective_config: VTCodeConfig,
    thread_handle: Option<ThreadRuntimeHandle>,
    archive_metadata: Option<crate::utils::session_archive::SessionArchiveMetadata>,
    stored_messages: Vec<Message>,
    recent_events: Vec<crate::exec::events::ThreadEvent>,
}

struct BackgroundCancellation {
    agent_name: String,
    exec_session_id: String,
}

enum BackgroundCancellationMode {
    Graceful,
    Force,
}

fn usable_background_parent_session_id(owner: Option<String>) -> Option<String> {
    owner.filter(|owner| !owner.is_empty())
}

struct PreparedBackgroundSubprocess {
    agent_name: String,
    record_id: String,
    prompt: String,
    desired_max_turns: Option<usize>,
    desired_model_override: Option<String>,
    desired_reasoning_override: Option<String>,
    max_turns: Option<usize>,
    model_override: Option<String>,
    reasoning_override: Option<String>,
    parent_session_id: String,
}

pub(super) struct BackgroundRunRequest {
    pub(super) agent_name: String,
    pub(super) stable_id: Option<String>,
    pub(super) restart_attempts: u8,
    pub(super) overrides: Option<BackgroundLaunchOverrides>,
}

struct BackgroundRunIdentity {
    spec: SubagentSpec,
    record_id: String,
    parent_session_id: String,
    session_id: String,
    exec_session_id: String,
    created_at: chrono::DateTime<Utc>,
    previous_record: Option<BackgroundRecord>,
}

struct BackgroundRunPlan {
    owner_session_id: Option<String>,
    spec: SubagentSpec,
    record_id: String,
    parent_session_id: String,
    session_id: String,
    exec_session_id: String,
    created_at: chrono::DateTime<Utc>,
    prompt: String,
    max_turns: Option<usize>,
    model_override: Option<String>,
    reasoning_override: Option<String>,
    restart_attempts: u8,
}

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::*;

impl SubagentController {
    /// Returns status entries for all tracked background subprocesses.
    pub async fn background_status_entries(&self) -> Vec<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        let invocation_owner = current_subagent_invocation_context().map(|context| context.parent_session_id);
        state
            .background_children
            .values()
            .filter(|record| invocation_owner.as_ref().is_none_or(|owner| &record.parent_session_id == owner))
            .map(BackgroundRecord::build_status_entry)
            .collect()
    }

    /// Returns a snapshot of a background subprocess including its preview output.
    pub async fn background_snapshot(&self, target: &str) -> Result<BackgroundSubprocessSnapshot> {
        let _ = self.refresh_background_processes().await?;

        let entry = {
            let state = self.state.read().await;
            let record = state
                .background_children
                .get(target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
            if current_subagent_invocation_context()
                .is_some_and(|context| context.parent_session_id != record.parent_session_id)
            {
                bail!("Unknown background subprocess {target}");
            }
            record.build_status_entry()
        };

        let preview = if entry.exec_session_id.is_empty() {
            String::new()
        } else {
            match self
                .config
                .exec_sessions
                .read_session_output(&entry.exec_session_id, false)
                .await
            {
                Ok(Some(output)) => extract_tail_lines(&output, SUBAGENT_PREVIEW_LINES),
                Ok(None) | Err(_) => {
                    if let Some(path) = entry.transcript_path.as_ref().or(entry.archive_path.as_ref()) {
                        load_archive_preview(path).await.unwrap_or_default()
                    } else {
                        String::new()
                    }
                }
            }
        };

        Ok(BackgroundSubprocessSnapshot { entry, preview })
    }

    /// Returns a caller-selected tail of background output.
    ///
    /// `None` requests the 200-line default. Values above
    /// [`MAX_SUBAGENT_OUTPUT_TAIL_LINES`] are rejected to keep ACP responses
    /// bounded; the existing [`Self::background_snapshot`] 24-line preview is
    /// unchanged.
    pub async fn background_output_tail(&self, target: &str, max_lines: Option<usize>) -> Result<String> {
        let max_lines = normalize_output_tail_lines(max_lines)?;
        let _ = self.refresh_background_processes().await?;

        let entry = {
            let state = self.state.read().await;
            state
                .background_children
                .get(target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?
                .build_status_entry()
        };

        if entry.exec_session_id.is_empty() {
            return Ok(String::new());
        }

        match self
            .config
            .exec_sessions
            .read_session_output(&entry.exec_session_id, false)
            .await
        {
            Ok(Some(output)) => Ok(extract_tail_lines(&output, max_lines)),
            Ok(None) | Err(_) => {
                if let Some(path) = entry.transcript_path.as_ref().or(entry.archive_path.as_ref()) {
                    load_archive_output_tail(path, max_lines).await
                } else {
                    Ok(String::new())
                }
            }
        }
    }

    /// Returns whether background subagents are enabled in the configuration.
    #[must_use]
    pub fn background_subagents_enabled(&self) -> bool {
        self.config.vt_cfg.subagents.background.enabled
    }

    /// Returns the configured default background agent name, if any.
    #[must_use]
    pub fn configured_default_background_agent(&self) -> Option<&str> {
        self.config
            .vt_cfg
            .subagents
            .background
            .default_agent
            .as_deref()
            .map(str::trim)
            .filter(|agent| !agent.is_empty())
    }

    /// Toggles the default background subagent between running and stopped.
    pub async fn toggle_default_background_subagent(&self) -> Result<BackgroundSubprocessEntry> {
        if !self.background_subagents_enabled() {
            bail!("Background subagents are disabled by configuration");
        }

        let agent_name = self
            .configured_default_background_agent()
            .ok_or_else(|| anyhow!("No default background subagent is configured"))?
            .to_string();
        let target_id = background_record_id(agent_name.as_str());
        let should_stop = {
            let state = self.state.read().await;
            state
                .background_children
                .get(&target_id)
                .is_some_and(|record| record.desired_enabled && record.status.is_active())
        };

        if should_stop {
            self.graceful_stop_background(&target_id).await
        } else {
            self.ensure_background_record_running(BackgroundRunRequest {
                agent_name,
                stable_id: Some(target_id),
                restart_attempts: 0,
                overrides: None,
            })
            .await
        }
    }

    /// Restarts background subagents that were previously enabled but are no longer running.
    pub async fn restore_background_subagents(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        let desired_records = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .filter(|record| record.desired_enabled)
                .map(|record| {
                    (
                        record.id.clone(),
                        record.agent_name.clone(),
                        record.exec_session_id.clone(),
                        record.restart_attempts,
                    )
                })
                .collect::<Vec<_>>()
        };

        for (record_id, agent_name, exec_session_id, restart_attempts) in desired_records {
            let is_live = !exec_session_id.is_empty()
                && self
                    .config
                    .exec_sessions
                    .snapshot_session(&exec_session_id)
                    .await
                    .ok()
                    .is_some_and(|snapshot| exec_session_is_running(&snapshot));

            if is_live || !self.config.vt_cfg.subagents.background.auto_restore {
                continue;
            }
            tracing::info!(
                agent_name = agent_name.as_str(),
                record_id = record_id.as_str(),
                "Restoring background subagent subprocess"
            );
            self.ensure_background_record_running(BackgroundRunRequest {
                agent_name,
                stable_id: Some(record_id),
                restart_attempts,
                overrides: None,
            })
            .await?;
        }

        self.refresh_background_processes().await
    }

    /// Refreshes the state of all background subprocesses and respawns as needed.
    pub async fn refresh_background_processes(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        let record_ids = {
            let state = self.state.read().await;
            state.background_children.keys().cloned().collect::<Vec<_>>()
        };

        let mut changed = false;
        for record_id in record_ids {
            let (snapshot_target, before_status, before_error) = {
                let state = self.state.read().await;
                let record = state.background_children.get(&record_id);
                (
                    record.map(|r| r.exec_session_id.clone()),
                    record.map(|r| r.status),
                    record.and_then(|r| r.error.clone()),
                )
            };

            let snapshot = if let Some(exec_session_id) = snapshot_target.as_ref()
                && !exec_session_id.is_empty()
            {
                self.config.exec_sessions.snapshot_session(exec_session_id).await.ok()
            } else {
                None
            };

            let respawn = self.update_background_record_state(&record_id, snapshot).await?;
            let respawning = respawn.is_some();

            if let Some((agent_name, stable_id, restart_attempts)) = respawn {
                self.ensure_background_record_running(BackgroundRunRequest {
                    agent_name,
                    stable_id: Some(stable_id),
                    restart_attempts,
                    overrides: None,
                })
                .await?;
            }

            let changed_this_record = {
                let state = self.state.read().await;
                state.background_children.get(&record_id).is_some_and(|r| {
                    r.status != before_status.unwrap_or(BackgroundSubprocessStatus::Starting) || r.error != before_error
                })
            };
            changed |= changed_this_record;

            if changed_this_record && !respawning {
                self.publish_background_status(&record_id).await;
            }

            self.refresh_background_archive_metadata(&record_id).await?;
        }

        if changed {
            self.save_background_state().await?;
        }
        Ok(self.background_status_entries().await)
    }

    async fn update_background_record_state(
        &self,
        record_id: &str,
        snapshot: Option<crate::tools::types::VTCodeExecSession>,
    ) -> Result<Option<(String, String, u8)>> {
        let mut state = self.state.write().await;
        let Some(record) = state.background_children.get_mut(record_id) else {
            return Ok(None);
        };
        record.updated_at = Utc::now();

        let Some(snapshot) = snapshot else {
            return Self::handle_missing_background_snapshot(record, &self.config);
        };

        record.pid = snapshot.child_pid;
        record.started_at = snapshot.started_at.or(record.started_at);

        match snapshot.lifecycle_state {
            Some(crate::tools::types::VTCodeSessionLifecycleState::Running) => {
                record.status = BackgroundSubprocessStatus::Running;
                record.ended_at = None;
                record.error = None;
            }
            Some(crate::tools::types::VTCodeSessionLifecycleState::Exited) | None => {
                record.ended_at.get_or_insert(Utc::now());
                if record.desired_enabled
                    && self.config.vt_cfg.subagents.background.auto_restore
                    && record.restart_attempts < 1
                {
                    let next_restart_attempt = record.restart_attempts.saturating_add(1);
                    record.restart_attempts = next_restart_attempt;
                    record.status = BackgroundSubprocessStatus::Starting;
                    tracing::warn!(
                        agent_name = record.agent_name.as_str(),
                        record_id = record.id.as_str(),
                        attempt = next_restart_attempt,
                        "Background subprocess exited unexpectedly; scheduling restart"
                    );
                    return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
                }
                Self::mark_background_record_stopped_or_error(record, &snapshot, &self.config);
            }
        }

        Ok(None)
    }

    fn handle_missing_background_snapshot(
        record: &mut BackgroundRecord,
        config: &SubagentControllerConfig,
    ) -> Result<Option<(String, String, u8)>> {
        if record.desired_enabled && config.vt_cfg.subagents.background.auto_restore {
            if record.restart_attempts < 1 {
                let next_restart_attempt = record.restart_attempts.saturating_add(1);
                record.restart_attempts = next_restart_attempt;
                record.status = BackgroundSubprocessStatus::Starting;
                tracing::warn!(
                    agent_name = record.agent_name.as_str(),
                    record_id = record.id.as_str(),
                    attempt = next_restart_attempt,
                    "Background subprocess is missing; scheduling restart"
                );
                return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
            }
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some("Background subprocess is not running".to_string());
            record.ended_at.get_or_insert(Utc::now());
        } else if !record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Stopped;
            record.ended_at.get_or_insert(Utc::now());
        }
        Ok(None)
    }

    fn mark_background_record_stopped_or_error(
        record: &mut BackgroundRecord,
        snapshot: &crate::tools::types::VTCodeExecSession,
        _config: &SubagentControllerConfig,
    ) {
        if record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some(match snapshot.exit_code {
                Some(exit_code) => format!("Background subprocess exited with code {exit_code}"),
                None => "Background subprocess exited unexpectedly".to_string(),
            });
        } else {
            record.status = BackgroundSubprocessStatus::Stopped;
        }
    }

    /// Gracefully stops a background subprocess by setting its desired state to disabled.
    pub async fn graceful_stop_background(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.cancel_background(target, BackgroundCancellationMode::Graceful).await
    }

    /// Force-cancels a background subprocess, closing its exec session immediately.
    pub async fn force_cancel_background(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.cancel_background(target, BackgroundCancellationMode::Force).await
    }

    async fn cancel_background(
        &self,
        target: &str,
        mode: BackgroundCancellationMode,
    ) -> Result<BackgroundSubprocessEntry> {
        let cancellation = self.begin_background_cancellation(target).await?;
        self.log_background_cancellation(target, &cancellation, &mode);
        if !cancellation.exec_session_id.is_empty() {
            match mode {
                BackgroundCancellationMode::Graceful => {
                    let _ = self.config.exec_sessions.terminate_session(&cancellation.exec_session_id).await;
                    let _ = self
                        .config
                        .exec_sessions
                        .prune_exited_session(&cancellation.exec_session_id)
                        .await;
                }
                BackgroundCancellationMode::Force => {
                    let _ = self.config.exec_sessions.close_session(&cancellation.exec_session_id).await;
                }
            }
        }
        self.finish_background_cancellation(target).await
    }

    fn log_background_cancellation(
        &self,
        target: &str,
        cancellation: &BackgroundCancellation,
        mode: &BackgroundCancellationMode,
    ) {
        let message = match mode {
            BackgroundCancellationMode::Graceful => "Gracefully stopping",
            BackgroundCancellationMode::Force => "Force cancelling",
        };
        tracing::info!(
            agent_name = cancellation.agent_name.as_str(),
            record_id = target,
            exec_session_id = cancellation.exec_session_id.as_str(),
            "{message} background subagent subprocess"
        );
    }

    async fn begin_background_cancellation(&self, target: &str) -> Result<BackgroundCancellation> {
        let mut state = self.state.write().await;
        let record = state
            .background_children
            .get_mut(target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        if current_subagent_invocation_context()
            .is_some_and(|context| context.parent_session_id != record.parent_session_id)
        {
            bail!("Unknown background subprocess {target}");
        }
        record.desired_enabled = false;
        record.status = BackgroundSubprocessStatus::Stopped;
        record.updated_at = Utc::now();
        record.ended_at = Some(Utc::now());
        Ok(BackgroundCancellation {
            agent_name: record.agent_name.clone(),
            exec_session_id: record.exec_session_id.clone(),
        })
    }

    async fn finish_background_cancellation(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.refresh_background_archive_metadata(target).await?;
        self.save_background_state().await?;
        let status = self.background_status_for(target).await?;
        self.publish_background_progress(status.clone()).await;
        Ok(status)
    }

    /// Returns a thread snapshot for a tracked child subagent by its target id.
    pub async fn snapshot_for_thread(&self, target: &str) -> Result<SubagentThreadSnapshot> {
        self.ensure_child_owned_by_invocation(target).await?;
        let captured = self.capture_subagent_thread(target).await?;
        let snapshot = self.materialize_thread_snapshot(&captured).await;
        Ok(SubagentThreadSnapshot {
            id: captured.id,
            session_id: captured.session_id,
            parent_thread_id: captured.parent_thread_id,
            agent_name: captured.agent_name,
            display_label: captured.display_label,
            status: captured.status,
            background: captured.background,
            created_at: captured.created_at,
            updated_at: captured.updated_at,
            archive_path: captured.archive_path,
            transcript_path: captured.transcript_path,
            effective_config: captured.effective_config,
            snapshot,
            recent_events: captured.recent_events,
        })
    }

    async fn capture_subagent_thread(&self, target: &str) -> Result<CapturedSubagentThread> {
        let state = self.state.read().await;
        let record = state
            .children
            .get(target)
            .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
        let effective_config = record
            .effective_config
            .clone()
            .ok_or_else(|| anyhow!("Subagent {target} does not have a captured runtime configuration yet"))?;
        Ok(CapturedSubagentThread {
            id: record.id.clone(),
            session_id: record.session_id.clone(),
            parent_thread_id: record.parent_thread_id.clone(),
            agent_name: record.spec.name.clone(),
            display_label: record.display_label.clone(),
            status: record.status,
            background: record.background,
            created_at: record.created_at,
            updated_at: record.updated_at,
            archive_path: record.archive_path.clone(),
            transcript_path: record.transcript_path.clone(),
            effective_config,
            thread_handle: record.thread_handle.clone(),
            archive_metadata: record.archive_metadata.clone(),
            stored_messages: record.stored_messages.clone(),
            recent_events: record
                .thread_handle
                .as_ref()
                .map(ThreadRuntimeHandle::recent_events)
                .unwrap_or_default(),
        })
    }

    async fn materialize_thread_snapshot(&self, captured: &CapturedSubagentThread) -> ThreadSnapshot {
        if let Some(handle) = captured.thread_handle.as_ref() {
            return handle.snapshot();
        }

        let archive_listing = match captured.archive_path.as_ref() {
            Some(path) if tokio::fs::metadata(path).await.is_ok() => load_session_listing(path).await.ok(),
            _ => None,
        };
        let metadata = archive_listing
            .as_ref()
            .map(|listing| listing.snapshot.metadata.clone())
            .or_else(|| captured.archive_metadata.clone())
            .or_else(|| {
                Some(crate::core::threads::build_thread_archive_metadata(
                    &self.config.workspace_root,
                    captured.effective_config.agent.default_model.as_str(),
                    captured.effective_config.agent.provider.as_str(),
                    captured.effective_config.agent.theme.as_str(),
                    captured.effective_config.agent.reasoning_effort.as_str(),
                ))
            });
        ThreadSnapshot {
            thread_id: ThreadId::new(captured.session_id.clone()),
            metadata,
            archive_listing,
            messages: captured.stored_messages.clone(),
            loaded_skills: Vec::new(),
            turn_in_flight: false,
        }
    }
    /// Spawns a background subprocess for a subagent marked `background: true`.
    pub async fn spawn_background_subprocess(
        &self,
        request: SpawnBackgroundSubprocessRequest,
    ) -> Result<BackgroundSubprocessEntry> {
        self.ensure_background_subprocess_enabled()?;
        let launch = self.prepare_background_subprocess(request).await?;
        let _ = self.refresh_background_processes().await?;
        if let Some(entry) = self.reusable_background_subprocess(&launch).await? {
            return Ok(entry);
        }

        let PreparedBackgroundSubprocess {
            agent_name,
            record_id,
            prompt,
            max_turns,
            model_override,
            reasoning_override,
            parent_session_id,
            ..
        } = launch;
        let launch_overrides = BackgroundLaunchOverrides {
            prompt: Some(prompt),
            max_turns,
            model_override,
            reasoning_override,
            parent_session_id: Some(parent_session_id),
        };
        self.ensure_background_record_running(BackgroundRunRequest {
            agent_name,
            stable_id: Some(record_id),
            restart_attempts: 0,
            overrides: Some(launch_overrides),
        })
        .await
    }

    fn ensure_background_subprocess_enabled(&self) -> Result<()> {
        if self.config.managed_background_runtime {
            bail!("managed background subprocesses cannot launch nested background subprocesses");
        }
        if !self.config.vt_cfg.subagents.background.enabled {
            bail!("Background subagents are disabled by configuration");
        }
        Ok(())
    }

    async fn prepare_background_subprocess(
        &self,
        mut request: SpawnBackgroundSubprocessRequest,
    ) -> Result<PreparedBackgroundSubprocess> {
        let delegation = self
            .prepare_delegation_context(
                request.agent_type.clone(),
                &mut request.items,
                &mut request.model,
                "spawn_background_subprocess",
            )
            .await?;
        let spec = self.resolve_requested_spec(delegation.requested_agent.as_deref()).await?;
        if !spec.background {
            bail!(
                "spawn_background_subprocess requires an agent with `background: true`; '{}' is a normal delegated child agent. Use spawn_agent instead.",
                spec.name
            );
        }
        let prompt = self.prepare_delegation_prompt(
            &spec,
            &delegation,
            &request.message,
            &request.items,
            "spawn_background_subprocess",
            "launching the background subprocess",
        )?;
        let desired_max_turns = normalize_background_child_max_turns(request.max_turns.or(spec.max_turns), true);
        let desired_model_override = request.model.clone().or_else(|| spec.model.clone());
        let desired_reasoning_override = request
            .reasoning_effort
            .clone()
            .or_else(|| spec.reasoning_effort.as_ref().map(|e| e.as_str().to_string()));
        let record_id = background_record_id(spec.name.as_str());

        Ok(PreparedBackgroundSubprocess {
            agent_name: spec.name.to_string(),
            record_id,
            prompt,
            desired_max_turns,
            desired_model_override,
            desired_reasoning_override,
            max_turns: request.max_turns,
            model_override: request.model,
            reasoning_override: request.reasoning_effort,
            parent_session_id: delegation.parent_session_id,
        })
    }

    async fn reusable_background_subprocess(
        &self,
        launch: &PreparedBackgroundSubprocess,
    ) -> Result<Option<BackgroundSubprocessEntry>> {
        let state = self.state.read().await;
        let Some(record) = state.background_children.get(&launch.record_id) else {
            return Ok(None);
        };
        if !record.desired_enabled || !record.status.is_active() {
            return Ok(None);
        }
        if current_subagent_invocation_context()
            .is_some_and(|context| context.parent_session_id != record.parent_session_id)
        {
            bail!("A background subprocess with this agent name is already active");
        }

        let conflicts = Self::active_background_launch_conflicts(record, launch);
        if !conflicts.is_empty() {
            bail!(
                "spawn_background_subprocess found active background subprocess '{}' with different {}. Stop or restart the existing subprocess before changing its launch settings.",
                launch.agent_name,
                conflicts.join(", "),
            );
        }
        Ok(Some(record.build_status_entry()))
    }

    pub(super) async fn background_status_for(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        let record = state
            .background_children
            .get(target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        Ok(record.build_status_entry())
    }

    pub(super) async fn ensure_background_record_running(
        &self,
        request: BackgroundRunRequest,
    ) -> Result<BackgroundSubprocessEntry> {
        let plan = self.prepare_background_run(request).await?;
        self.record_background_run_starting(&plan).await;
        let metadata = self.spawn_background_exec_session(&plan).await?;
        self.record_background_run_started(&plan, metadata).await?;
        self.save_background_state().await?;
        self.background_status_for(&plan.record_id).await
    }

    async fn resolve_background_run_identity(&self, request: &BackgroundRunRequest) -> Result<BackgroundRunIdentity> {
        let spec = self
            .resolve_requested_spec(Some(request.agent_name.as_str()))
            .await
            .with_context(|| format!("Failed to resolve background subagent '{}'", request.agent_name))?;
        let record_id = request
            .stable_id
            .clone()
            .unwrap_or_else(|| background_record_id(request.agent_name.as_str()));
        let previous_record = {
            let state = self.state.read().await;
            state.background_children.get(&record_id).cloned()
        };
        let previous_parent_session_id = usable_background_parent_session_id(
            previous_record.as_ref().map(|record| record.parent_session_id.clone()),
        );
        let current_parent_session_id = self.parent_session_id.read().await.clone();
        let requested_parent_session_id = usable_background_parent_session_id(
            request
                .overrides
                .as_ref()
                .and_then(|overrides| overrides.parent_session_id.clone()),
        );
        let parent_session_id = requested_parent_session_id
            .or(previous_parent_session_id)
            .unwrap_or(current_parent_session_id);
        let session_id = format!(
            "{}-{}-{}",
            sanitize_component(parent_session_id.as_str()),
            sanitize_component(record_id.as_str()),
            Utc::now().format("%Y%m%dT%H%M%S%3fZ")
        );
        let exec_session_id = format!("exec-{session_id}");
        let created_at = previous_record.as_ref().map_or_else(Utc::now, |record| record.created_at);
        Ok(BackgroundRunIdentity {
            spec,
            record_id,
            parent_session_id,
            session_id,
            exec_session_id,
            created_at,
            previous_record,
        })
    }

    async fn prepare_background_run(&self, request: BackgroundRunRequest) -> Result<BackgroundRunPlan> {
        let identity = self.resolve_background_run_identity(&request).await?;
        let previous_record = identity.previous_record.as_ref();
        let previous_prompt = previous_record.map_or_else(String::new, |record| record.prompt.clone());
        let prompt = request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.prompt.clone())
            .filter(|value| !value.trim().is_empty())
            .or_else(|| (!previous_prompt.trim().is_empty()).then_some(previous_prompt))
            .or_else(|| identity.spec.initial_prompt.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "You are the VT Code background subagent `{}`. Summarize readiness briefly, inspect the workspace at a high level, then remain idle until the process is terminated.",
                    identity.spec.name
                )
            });
        let max_turns = normalize_background_child_max_turns(
            request
                .overrides
                .as_ref()
                .and_then(|overrides| overrides.max_turns)
                .or_else(|| previous_record.and_then(|record| record.max_turns))
                .or(identity.spec.max_turns),
            true,
        );
        let model_override = request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.model_override.clone())
            .or_else(|| previous_record.and_then(|record| record.model_override.clone()))
            .or_else(|| identity.spec.model.clone());
        let reasoning_override = request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.reasoning_override.clone())
            .or_else(|| previous_record.and_then(|record| record.reasoning_override.clone()))
            .or_else(|| {
                identity
                    .spec
                    .reasoning_effort
                    .as_ref()
                    .map(|effort| effort.as_str().to_string())
            });

        let owner_session_id = match previous_record {
            Some(record) => record.owner_session_id.clone(),
            None => Some(identity.parent_session_id.clone()),
        };
        Ok(BackgroundRunPlan {
            owner_session_id,
            spec: identity.spec,
            record_id: identity.record_id,
            parent_session_id: identity.parent_session_id,
            session_id: identity.session_id,
            exec_session_id: identity.exec_session_id,
            created_at: identity.created_at,
            prompt,
            max_turns,
            model_override,
            reasoning_override,
            restart_attempts: request.restart_attempts,
        })
    }

    async fn record_background_run_starting(&self, plan: &BackgroundRunPlan) {
        {
            let mut state = self.state.write().await;
            state.background_children.insert(
                plan.record_id.clone(),
                BackgroundRecord {
                    owner_session_id: plan.owner_session_id.clone(),
                    id: plan.record_id.clone(),
                    agent_name: plan.spec.name.clone(),
                    display_label: subagent_display_label(&plan.spec),
                    description: plan.spec.description.clone(),
                    source: plan.spec.source.label(),
                    color: plan.spec.color.clone(),
                    session_id: plan.session_id.clone(),
                    parent_session_id: plan.parent_session_id.clone(),
                    exec_session_id: plan.exec_session_id.clone(),
                    desired_enabled: true,
                    status: BackgroundSubprocessStatus::Starting,
                    created_at: plan.created_at,
                    updated_at: Utc::now(),
                    started_at: None,
                    ended_at: None,
                    pid: None,
                    prompt: plan.prompt.clone(),
                    summary: Some("Starting background subagent".to_string()),
                    error: None,
                    archive_path: None,
                    transcript_path: None,
                    max_turns: plan.max_turns,
                    model_override: plan.model_override.clone(),
                    reasoning_override: plan.reasoning_override.clone(),
                    restart_attempts: plan.restart_attempts,
                },
            );
        }
        self.publish_background_status(&plan.record_id).await;
    }

    async fn spawn_background_exec_session(
        &self,
        plan: &BackgroundRunPlan,
    ) -> Result<crate::tools::types::VTCodeExecSession> {
        let launch = build_background_launch_spec(
            &self.config.workspace_root,
            plan.spec.name.as_str(),
            plan.parent_session_id.as_str(),
            plan.session_id.as_str(),
            plan.prompt.as_str(),
            plan.max_turns,
            plan.model_override.as_deref(),
            plan.reasoning_override.as_deref(),
        )?;
        let metadata = if launch.use_pty {
            self.config
                .exec_sessions
                .create_pty_session(
                    plan.exec_session_id.clone().into(),
                    launch.command,
                    self.config.workspace_root.clone(),
                    PtySize {
                        rows: 24,
                        cols: 80,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                    hashbrown::HashMap::new(),
                    None,
                )
                .await
        } else {
            self.config
                .exec_sessions
                .create_pipe_session(
                    plan.exec_session_id.clone().into(),
                    launch.command,
                    self.config.workspace_root.clone(),
                    hashbrown::HashMap::new(),
                )
                .await
        }
        .with_context(|| format!("Failed to spawn background subprocess for subagent '{}'", plan.spec.name))?;

        tracing::info!(
            agent_name = plan.spec.name.as_str(),
            record_id = plan.record_id.as_str(),
            exec_session_id = plan.exec_session_id.as_str(),
            pid = metadata.child_pid,
            "Spawned background subagent subprocess"
        );
        Ok(metadata)
    }

    async fn record_background_run_started(
        &self,
        plan: &BackgroundRunPlan,
        metadata: crate::tools::types::VTCodeExecSession,
    ) -> Result<()> {
        {
            let mut state = self.state.write().await;
            let record = state
                .background_children
                .get_mut(&plan.record_id)
                .ok_or_else(|| anyhow!("Unknown background subprocess {}", plan.record_id))?;
            record.exec_session_id = plan.exec_session_id.clone();
            record.pid = metadata.child_pid;
            record.started_at = metadata.started_at;
            record.status = BackgroundSubprocessStatus::Running;
            record.updated_at = Utc::now();
            record.ended_at = None;
            record.error = None;
            record.summary = Some("Background subagent is running".to_string());
        }
        self.publish_background_status(&plan.record_id).await;
        Ok(())
    }

    pub(super) async fn refresh_background_archive_metadata(&self, target: &str) -> Result<()> {
        let session_id = {
            let state = self.state.read().await;
            state
                .background_children
                .get(target)
                .map(|record| record.session_id.clone())
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?
        };

        if let Some(listing) = find_session_by_identifier(&session_id).await? {
            let mut state = self.state.write().await;
            if let Some(record) = state.background_children.get_mut(target) {
                record.archive_path = Some(listing.path.clone());
                record.transcript_path = Some(listing.path);
            }
        }

        Ok(())
    }

    fn active_background_launch_conflicts(
        record: &BackgroundRecord,
        launch: &PreparedBackgroundSubprocess,
    ) -> Vec<&'static str> {
        let mut conflicts = Vec::new();
        if record.prompt != launch.prompt {
            conflicts.push("prompt");
        }
        if record.max_turns != launch.desired_max_turns {
            conflicts.push("max_turns");
        }
        if record.model_override.as_deref() != launch.desired_model_override.as_deref() {
            conflicts.push("model");
        }
        if record.reasoning_override.as_deref() != launch.desired_reasoning_override.as_deref() {
            conflicts.push("reasoning_effort");
        }
        conflicts
    }
}

#[cfg(test)]
mod background_owner_tests {
    use super::usable_background_parent_session_id;

    #[test]
    fn empty_legacy_parent_owner_falls_back_to_the_current_session() {
        let previous_owner = usable_background_parent_session_id(Some(String::new()));
        let current_owner = "current-session".to_owned();

        assert_eq!(previous_owner.unwrap_or(current_owner), "current-session");
    }
}
