//! Plans a background launch, starts its exec session, and records its outcome.

use super::*;

use super::controller_background_query::{resolve_background_target, resolve_background_target_mut};
use super::controller_background_settlement::mark_background_ledger_write_pending;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Builds an exec-session id that no other launch in this process can repeat.
///
/// The session component already carries a millisecond timestamp, but two
/// launches of one record in the same millisecond derived the same id, and the
/// second spawn then failed on the duplicate-session check while the first was
/// still starting — a failure settlement could blame on the winner's live
/// record. A process-wide counter makes every launch distinct, and the process
/// id keeps one process's sequence from colliding with another's.
pub(super) fn background_exec_session_id(session_id: &str) -> String {
    static LAUNCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = LAUNCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("exec-{session_id}-{:x}-{sequence:x}", std::process::id())
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

impl SubagentController {
    pub(super) async fn background_status_for(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        let record = resolve_background_target(&state, target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        Ok(record.build_status_entry())
    }

    pub(super) async fn ensure_background_record_running(
        &self,
        request: BackgroundRunRequest,
    ) -> Result<BackgroundSubprocessEntry> {
        let mut plan = self.prepare_background_run(request).await?;
        loop {
            self.record_background_run_starting(&plan).await;
            let metadata = match self.spawn_background_exec_session(&plan).await {
                Ok(metadata) => metadata,
                Err(error) => {
                    self.settle_failed_background_run(&plan, &error).await;
                    // A spawn failure is the only thing that spends the
                    // restart allowance, and `settle_failed_background_run`
                    // leaves the record `Starting` exactly when the allowance
                    // remains. Nothing else observes that state as a launch to
                    // drive, so the retry has to happen here; otherwise the
                    // record stays `Starting` forever and is reported as an
                    // active process that will never run.
                    let Some(retry) = self.background_launch_retry_request(&plan).await else {
                        return Err(error);
                    };
                    tracing::warn!(
                        agent_name = retry.agent_name.as_str(),
                        record_id = plan.record_id.as_str(),
                        attempt = retry.restart_attempts,
                        error = %error,
                        "Background launch failed; retrying once"
                    );
                    plan = self.prepare_background_run(retry).await?;
                    continue;
                }
            };
            if !self.record_background_run_started(&plan, metadata).await? {
                // The record stopped wanting this launch while the spawn was in
                // flight. The session exists now and nothing records it, so
                // close it rather than leave an untracked process running.
                self.close_background_exec_session(&plan.exec_session_id).await;
                return self.background_status_for(&plan.record_id).await;
            }
            // The record is committed in memory but the ledger is not. A
            // caller told the launch failed must not later have that record
            // reused as if it were durable, so the failure is remembered and
            // the next refresh retries the write before reuse can observe it.
            if let Err(error) = self.save_background_state().await {
                mark_background_ledger_write_pending(&self.state);
                return Err(error);
            }
            return self.background_status_for(&plan.record_id).await;
        }
    }

    /// Builds the single retry a failed launch is still allowed, if any.
    ///
    /// The allowance is spent here, under the write lock, so a concurrent
    /// refresh cannot schedule the same replacement from an unchanged record.
    /// The retry is offered only while the failed launch still owns the record:
    /// once `settle_failed_background_run` has settled it, or a cancellation
    /// has cleared `desired_enabled`, the failure is terminal and a retry would
    /// undo that outcome.
    async fn background_launch_retry_request(&self, plan: &BackgroundRunPlan) -> Option<BackgroundRunRequest> {
        if !self.config.vt_cfg.subagents.background.auto_restore {
            return None;
        }
        let next_restart_attempt = {
            let mut state = self.state.write().await;
            let record = state.background_children.get_mut(&plan.record_id)?;
            if !failed_launch_still_owns_record(record, plan.exec_session_id.as_str()) || record.restart_attempts >= 1 {
                return None;
            }
            record.restart_attempts = record.restart_attempts.saturating_add(1);
            record.updated_at = Utc::now();
            record.restart_attempts
        };
        Some(BackgroundRunRequest {
            agent_name: plan.spec.name.as_str().to_string(),
            stable_id: Some(plan.record_id.clone()),
            restart_attempts: next_restart_attempt,
            overrides: None,
        })
    }

    async fn resolve_background_run_identity(&self, request: &BackgroundRunRequest) -> Result<BackgroundRunIdentity> {
        let spec = self
            .resolve_requested_spec(Some(request.agent_name.as_str()))
            .await
            .with_context(|| format!("Failed to resolve background subagent '{}'", request.agent_name))?;
        let requested_record_id = request.stable_id.clone().unwrap_or_else(|| {
            background_record_id_for_owner(request.agent_name.as_str(), self.background_owner_session_id.as_deref())
        });
        // A client can address an existing record by the legacy id an older
        // release persisted, and a relaunch must update that record rather than
        // create a second one under the id the client sent.
        let previous_record = {
            let state = self.state.read().await;
            resolve_background_target(&state, &requested_record_id).cloned()
        };
        let record_id = previous_record.as_ref().map_or(requested_record_id, |record| record.id.clone());
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
        let exec_session_id = background_exec_session_id(&session_id);
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
                    "You are the Diapason background subagent `{}`. Summarize readiness briefly, inspect the workspace at a high level, then remain idle until the process is terminated.",
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

        // An owner-scoped controller tags its own records so sibling sessions
        // in one workspace cannot adopt or overwrite them. An unscoped
        // controller manages the whole workspace ledger, so the records it
        // creates are workspace-global and ownerless; giving them a per-run
        // owner would change their canonical id on every restart.
        let owner_session_id = match previous_record {
            Some(record) => record.owner_session_id.clone(),
            None => self.background_owner_session_id.clone(),
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
                    colour: plan.spec.colour.clone(),
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
    ) -> Result<crate::tools::types::DiapasonExecSession> {
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

    /// Commits a spawned launch to its record, unless that record no longer wants it.
    ///
    /// The spawn runs without the state lock, so any controller action can
    /// change the record while it is in flight: a cancellation can clear
    /// `desired_enabled`, a refresh can schedule a replacement, and a stop
    /// another controller persisted can be adopted. Writing this launch over
    /// that change would resurrect a cancelled task, attach one process's
    /// metadata to a different launch, or keep a process alive that no record
    /// names. The commit re-checks the record under the write lock and reports
    /// `false` when it declined, leaving the record as the concurrent action
    /// left it; the caller closes the session it created. A launch also refuses
    /// an exec-session id another record holds: sharing one id would make the
    /// two indistinguishable to pruning and cancellation, so the launch is
    /// declined and the duplicate session closed.
    async fn record_background_run_started(
        &self,
        plan: &BackgroundRunPlan,
        metadata: crate::tools::types::DiapasonExecSession,
    ) -> Result<bool> {
        self.commit_background_run_started(
            &plan.record_id,
            &plan.exec_session_id,
            metadata.child_pid,
            metadata.started_at,
        )
        .await
    }

    pub(super) async fn refresh_background_archive_metadata(&self, target: &str) -> Result<()> {
        let session_id = {
            let state = self.state.read().await;
            resolve_background_target(&state, target)
                .map(|record| record.session_id.clone())
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?
        };

        if let Some(listing) = find_session_by_identifier(&session_id).await? {
            let mut state = self.state.write().await;
            attach_background_archive_paths(&mut state, target, &session_id, &listing.path);
        }

        Ok(())
    }
}

/// Attaches resolved archive paths to the launch that named `session_id`.
///
/// The lookup ran without the state lock, and a restart can replace the
/// record's session meanwhile. These paths belong to the launch that named the
/// session the lookup resolved, so they are attached only while the record still
/// names it; the replacement records its own.
pub(super) fn attach_background_archive_paths(
    state: &mut ControllerState,
    target: &str,
    session_id: &str,
    archive_path: &Path,
) {
    if let Some(record) = resolve_background_target_mut(state, target)
        && record.session_id == session_id
    {
        record.archive_path = Some(archive_path.to_path_buf());
        record.transcript_path = Some(archive_path.to_path_buf());
    }
}
