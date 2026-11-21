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
mod effective_reuse_tests {
    //! Pins the effective-settings rule the reuse check compares.

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::*;

    fn test_config(workspace_root: PathBuf) -> SubagentControllerConfig {
        let vt_cfg = DiapasonConfig::default();
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
        let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
        SubagentControllerConfig {
            workspace_root,
            parent_session_id: "parent-session".to_string(),
            parent_model: "gpt-5.6-sol".to_string(),
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

    fn background_record(model_override: Option<&str>) -> BackgroundRecord {
        BackgroundRecord {
            id: "background-worker".to_string(),
            owner_session_id: None,
            agent_name: "worker".to_string(),
            display_label: "worker".to_string(),
            description: "worker".to_string(),
            source: "builtin".to_string(),
            colour: None,
            session_id: "session-background-worker".to_string(),
            parent_session_id: "parent-session".to_string(),
            exec_session_id: "exec-live".to_string(),
            desired_enabled: true,
            status: BackgroundSubprocessStatus::Running,
            created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
            updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
            started_at: None,
            ended_at: None,
            pid: None,
            prompt: "work".to_string(),
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
            max_turns: None,
            model_override: model_override.map(str::to_owned),
            reasoning_override: None,
            restart_attempts: 0,
        }
    }

    /// Builds the request half of a launch whose spec defaults to `spec_model`.
    fn prepared_launch(request_model: Option<&str>, spec_model: Option<&str>) -> PreparedBackgroundSubprocess {
        PreparedBackgroundSubprocess {
            agent_name: "worker".to_string(),
            record_id: "background-worker".to_string(),
            prompt: "work".to_string(),
            desired_max_turns: None,
            desired_model_override: spec_model.map(str::to_owned),
            desired_reasoning_override: None,
            max_turns: None,
            model_override: request_model.map(str::to_owned),
            reasoning_override: None,
            parent_session_id: "parent-session".to_string(),
        }
    }

    async fn active_record_controller(workspace: &TempDir, model_override: Option<&str>) -> SubagentController {
        let controller = SubagentController::new(test_config(workspace.path().to_path_buf()))
            .await
            .expect("controller");
        let record = background_record(model_override);
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(record.id.clone(), record);
        }
        controller
    }

    /// A request that omits a setting must use the record's value, as the plan
    /// does, instead of conflicting with the spec's default.
    ///
    /// The record carries model X while the spec defaults to model Y. A request
    /// without a model resolves to X through the plan's
    /// request-then-record-then-spec precedence, so the active launch is
    /// compatible and must be reused rather than rejected as a different model.
    #[tokio::test]
    async fn a_request_without_a_model_reuses_the_active_records_model() {
        let temp = TempDir::new().expect("tempdir");
        let controller = active_record_controller(&temp, Some("model-x")).await;

        let reused = controller
            .reusable_background_subprocess(&prepared_launch(None, Some("model-y")))
            .await
            .expect("a request that omits the model reuses the active record");

        let entry = reused.expect("the active launch satisfies the request");
        assert_eq!(entry.id, "background-worker");
        assert_eq!(entry.exec_session_id, "exec-live", "reuse must not replace the running launch");

        controller.signal_shutdown().await;
    }

    /// An explicit request for a different model is still a conflict.
    ///
    /// Only an omitted setting falls back to the record; an explicit request
    /// outranks it, exactly as in the plan.
    #[tokio::test]
    async fn an_explicitly_different_model_still_conflicts() {
        let temp = TempDir::new().expect("tempdir");
        let controller = active_record_controller(&temp, Some("model-x")).await;

        let error = controller
            .reusable_background_subprocess(&prepared_launch(Some("model-z"), Some("model-z")))
            .await
            .expect_err("a request for different work must not reuse the active record");

        assert!(error.to_string().contains("different model"), "the conflict names the model: {error:#}");

        controller.signal_shutdown().await;
    }
}
