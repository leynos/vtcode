//! Prepares and reuses an externally requested background subprocess launch.

use super::*;

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

impl SubagentController {
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
            expected_exec_session_id: None,
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
        let record_id = background_record_id_for_owner(spec.name.as_str(), self.background_owner_session_id.as_deref());

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
        // An explicit request can finish `refresh_background_processes` just
        // before a matching launch commits `Running`. Answering from memory then
        // reports a record whose ledger write is still owed, so a failure of
        // that write would never be visible to this caller. The marker is
        // checked under the same lock that selects the record; when it is
        // raised, the lock is released, the write is retried, and the record is
        // selected again once the write it owed has succeeded.
        let state = self.state.read().await;
        if pending_background_ledger_write_generation(&self.state).is_none() {
            return Self::reusable_background_from_state(launch, &state);
        }
        drop(state);
        self.retry_owed_background_ledger_write().await?;
        let state = self.state.read().await;
        if pending_background_ledger_write_generation(&self.state).is_some() {
            // Another write failed while the retry ran. Reporting the record now
            // would still answer for one the ledger may never hold.
            bail!(
                "Background subprocess '{}' still owes its ledger write; retry once it has been persisted",
                launch.agent_name
            );
        }
        Self::reusable_background_from_state(launch, &state)
    }

    /// Selects the reusable record for `launch` from the state it read.
    ///
    /// The caller holds the state lock and has checked that no ledger write is
    /// owed, so a matching record it reports is one the ledger already holds.
    fn reusable_background_from_state(
        launch: &PreparedBackgroundSubprocess,
        state: &ControllerState,
    ) -> Result<Option<BackgroundSubprocessEntry>> {
        let Some(record) = state.background_children.get(&launch.record_id) else {
            return Ok(None);
        };
        if !record.desired_enabled || !record.status.is_active() {
            return Ok(None);
        }
        // Reuse answers with the existing launch and drops the parent this
        // request resolved, so the rule compares that resolved parent as well
        // as the ambient invocation context: a caller outside a tool invocation
        // has no context to compare, and must not reuse a foreign launch either.
        // A parentless record has no owner to conflict with, so the ambient
        // comparison exempts it and the rebind below takes it onto the caller.
        if reuse_is_for_another_parent(&record.parent_session_id, &launch.parent_session_id)
            || reuse_is_refused_by_the_invocation_context(&record.parent_session_id)
        {
            bail!("A background subprocess with this agent name is already active");
        }
        // A parentless record routes its progress nowhere, so this pre-lock
        // selector cannot answer for it: the rebind needs the write lock. The
        // reservation path below takes that lock, moves the record onto the
        // requesting session, and reuses it there, so no observer is skipped.
        if reuse_needs_parent_rebind(&record.parent_session_id, &launch.parent_session_id) {
            return Ok(None);
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

    /// Names the settings on which `record` differs from the plan `launch`
    /// would become.
    ///
    /// The plan resolves each setting as the request's value, then the launch
    /// it replaces (this record), then the spec's default. Reuse must compare
    /// those same effective values: comparing the request against the spec
    /// alone reports a conflict for a request that merely omits a setting the
    /// active record already carries, although the plan would keep the
    /// record's value and reuse it.
    fn active_background_launch_conflicts(
        record: &BackgroundRecord,
        launch: &PreparedBackgroundSubprocess,
    ) -> Vec<&'static str> {
        // The plan stores a normalized max-turns bound, so the record's value
        // is normalized the same way before it stands in for an omitted
        // request. A request that names one keeps its own normalized value.
        let max_turns = if launch.max_turns.is_some() {
            launch.desired_max_turns
        } else {
            normalize_background_child_max_turns(record.max_turns, true).or(launch.desired_max_turns)
        };
        let model_override = launch
            .model_override
            .as_deref()
            .or(record.model_override.as_deref())
            .or(launch.desired_model_override.as_deref());
        let reasoning_override = launch
            .reasoning_override
            .as_deref()
            .or(record.reasoning_override.as_deref())
            .or(launch.desired_reasoning_override.as_deref());
        background_launch_conflicts(record, launch.prompt.as_str(), max_turns, model_override, reasoning_override)
    }
}

#[cfg(test)]
mod effective_reuse_tests;
