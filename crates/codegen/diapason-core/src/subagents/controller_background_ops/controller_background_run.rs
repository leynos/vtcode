//! Plans a background launch, starts its exec session, and records its outcome.

use super::*;

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
        let metadata = match self.spawn_background_exec_session(&plan).await {
            Ok(metadata) => metadata,
            Err(error) => {
                self.settle_failed_background_run(&plan, &error).await;
                return Err(error);
            }
        };
        self.record_background_run_started(&plan, metadata).await?;
        self.save_background_state().await?;
        self.background_status_for(&plan.record_id).await
    }

    async fn resolve_background_run_identity(&self, request: &BackgroundRunRequest) -> Result<BackgroundRunIdentity> {
        let spec = self
            .resolve_requested_spec(Some(request.agent_name.as_str()))
            .await
            .with_context(|| format!("Failed to resolve background subagent '{}'", request.agent_name))?;
        let record_id = request.stable_id.clone().unwrap_or_else(|| {
            background_record_id_for_owner(request.agent_name.as_str(), self.background_owner_session_id.as_deref())
        });
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

    async fn record_background_run_started(
        &self,
        plan: &BackgroundRunPlan,
        metadata: crate::tools::types::DiapasonExecSession,
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
}
