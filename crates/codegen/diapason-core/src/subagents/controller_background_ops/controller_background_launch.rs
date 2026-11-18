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
