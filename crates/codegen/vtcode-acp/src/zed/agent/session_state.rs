use super::ZedAgent;
use crate::acp;
use crate::workspace::{DefaultWorkspaceTrustSynchronizer, WorkspaceTrustSynchronizer};
use anyhow::Context;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;
use vtcode_commons::fs::canonicalize_with_context_async;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_core::config::models::{ModelId, Provider};
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::core::threads::{ThreadBootstrap, build_thread_archive_metadata};
use vtcode_core::hooks::{LifecycleHookEngine, SessionEndReason, SessionStartTrigger};
use vtcode_core::llm::ModelResolver;
use vtcode_core::llm::factory::get_factory;
use vtcode_core::llm::provider::{FinishReason, Message, MessageRole};
use vtcode_core::utils::session_archive::{
    SessionArchive, SessionListing, SessionMessage, SessionProgressArgs, find_session_by_identifier,
    history_persistence_enabled, list_recent_sessions, session_listing_matches_workspace, session_workspace_path,
};

use super::super::constants::SESSION_PREFIX;
use super::super::helpers::session_config_options;
use super::super::types::{SessionData, SessionHandle};

async fn canonical_session_workspace(requested_workspace: &std::path::Path) -> Result<std::path::PathBuf, acp::Error> {
    if !requested_workspace.is_absolute() {
        return Err(acp::Error::invalid_params().data("ACP session cwd must be an absolute directory"));
    }
    let workspace = canonicalize_with_context_async(requested_workspace, "ACP session cwd")
        .await
        .map_err(|error| {
            acp::Error::invalid_params()
                .data(format!("Unable to resolve ACP session cwd '{}': {error}", requested_workspace.display()))
        })?;
    let metadata = tokio::fs::metadata(&workspace).await.map_err(|error| {
        acp::Error::invalid_params()
            .data(format!("Unable to inspect ACP session cwd '{}': {error}", requested_workspace.display()))
    })?;
    if !metadata.is_dir() {
        return Err(acp::Error::invalid_params()
            .data(format!("ACP session cwd '{}' is not a directory", requested_workspace.display())));
    }
    Ok(workspace)
}

impl ZedAgent {
    const SESSION_LIST_PAGE_SIZE: usize = 100;

    pub(crate) async fn run_session_start_hooks(&self, session: &SessionHandle) -> anyhow::Result<()> {
        if !session.mark_session_started() {
            return Ok(());
        }

        session.update_transcript_path().await;
        let Some(hooks) = session.lifecycle_hooks() else {
            return Ok(());
        };
        let outcome = hooks.run_session_start().await?;
        for message in outcome.messages {
            warn!(level = ?message.level, message = %message.text, "ACP SessionStart hook");
        }
        for context in outcome.additional_context {
            if !context.trim().is_empty() {
                self.push_message(session, Message::system(context));
            }
        }
        Ok(())
    }

    pub(crate) async fn run_session_end_hooks(&self, reason: SessionEndReason) {
        let sessions = self
            .sessions
            .lock()
            .map(|sessions| sessions.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for session in sessions {
            if !session.mark_session_ended() {
                continue;
            }
            let Some(hooks) = session.lifecycle_hooks() else {
                continue;
            };
            let turn_id = session
                .data
                .lock()
                .map(|data| data.session_id.to_string())
                .unwrap_or_else(|_| "acp-session".to_string());
            match hooks.run_session_end(&turn_id, reason).await {
                Ok(messages) => {
                    for message in messages {
                        warn!(level = ?message.level, message = %message.text, "ACP SessionEnd hook");
                    }
                }
                Err(error) => warn!(%error, "ACP SessionEnd hooks failed"),
            }
        }
    }

    fn session_reasoning_effort_for_thread(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
    ) -> ReasoningEffortLevel {
        thread
            .metadata()
            .and_then(|metadata| ReasoningEffortLevel::parse(&metadata.reasoning_effort))
            .unwrap_or(self.config.reasoning_effort)
    }

    fn sync_thread_reasoning_effort(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        reasoning_effort: ReasoningEffortLevel,
    ) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.reasoning_effort = reasoning_effort.as_str().to_string();
            thread.replace_metadata(Some(metadata));
        }
    }

    pub(super) fn merge_session_acp_meta(&self, session: &SessionHandle, acp_meta: Option<acp::Meta>) {
        debug!(acp_meta_key_count = acp_meta.as_ref().map_or(0, acp::Meta::len), "Received ACP request metadata");
        let Some(acp_meta) = acp_meta.filter(|meta| !meta.is_empty()) else {
            return;
        };
        let Ok(data) = session.data.lock() else {
            return;
        };
        if let Some(mut metadata) = data.thread.metadata() {
            metadata.acp_meta.get_or_insert_default().extend(acp_meta);
            data.thread.replace_metadata(Some(metadata));
        }
    }

    fn session_provider_for_thread(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle) -> String {
        thread
            .metadata()
            .map(|metadata| metadata.provider)
            .unwrap_or_else(|| self.config.provider.clone())
    }

    fn session_model_for_thread(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle) -> String {
        thread
            .metadata()
            .map(|metadata| metadata.model)
            .unwrap_or_else(|| self.config.model.clone())
    }

    fn session_primary_agent_for_thread(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        primary_agents: &super::super::helpers::PrimaryAgentCatalog,
    ) -> String {
        thread
            .metadata()
            .and_then(|metadata| metadata.primary_agent)
            .and_then(|primary_agent| primary_agents.resolve_id(&primary_agent))
            .map(ToString::to_string)
            .unwrap_or_else(|| primary_agents.default_id().to_string())
    }

    fn sync_thread_primary_agent(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle, primary_agent: &str) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.primary_agent = Some(primary_agent.to_string());
            thread.replace_metadata(Some(metadata));
        }
    }

    fn sync_thread_provider_and_model(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        provider: &str,
        model: &str,
    ) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.provider = provider.to_string();
            metadata.model = model.to_string();
            thread.replace_metadata(Some(metadata));
        }
    }

    fn model_supports_thought_level(&self, provider: &str, model: &str) -> bool {
        ModelResolver::resolve(Some(provider), model, &[], None)
            .map(|resolved| resolved.reasoning_supported())
            .unwrap_or(false)
    }

    fn build_session_handle(
        &self,
        session_id: acp::SessionId,
        thread: vtcode_core::core::threads::ThreadRuntimeHandle,
        trigger: SessionStartTrigger,
    ) -> SessionHandle {
        self.build_session_handle_with_archive(session_id, thread, None, trigger, None)
    }

    fn build_session_handle_with_archive(
        &self,
        session_id: acp::SessionId,
        thread: vtcode_core::core::threads::ThreadRuntimeHandle,
        archive: Option<SessionArchive>,
        trigger: SessionStartTrigger,
        workspace_runtime: Option<Arc<super::SessionWorkspaceRuntime>>,
    ) -> SessionHandle {
        let reasoning_effort = self.session_reasoning_effort_for_thread(&thread);
        let provider = self.session_provider_for_thread(&thread);
        let model = self.session_model_for_thread(&thread);
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let primary_agent = self.session_primary_agent_for_thread(&thread, primary_agents);
        let hook_workspace = workspace_runtime
            .as_ref()
            .map_or_else(|| self.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
        let lifecycle_hooks = self.vt_config.as_ref().and_then(|config| {
            match LifecycleHookEngine::new_with_session(
                hook_workspace,
                &config.hooks,
                trigger,
                session_id.0.to_string(),
            ) {
                Ok(hooks) => hooks,
                Err(error) => {
                    warn!(%error, "Failed to initialize ACP lifecycle hooks");
                    None
                }
            }
        });
        SessionHandle {
            data: Arc::new(Mutex::new(SessionData {
                session_id,
                thread,
                archive,
                workspace_runtime,
                tool_notice_sent: std::sync::atomic::AtomicBool::new(false),
                primary_agent,
                reasoning_effort,
                provider,
                model,
                last_tool_call_at: None,
                auto_compact_suppressed: 0,
                lifecycle_hooks,
                session_started: false,
                session_ended: false,
                task_lifecycle_forwarder: None,
            })),
            cancellation: super::super::types::SessionCancellation::default(),
        }
    }

    pub(crate) fn register_session(&self) -> acp::SessionId {
        let raw_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{raw_id}")));
        let metadata = build_thread_archive_metadata(
            self.config.workspace.as_path(),
            &self.config.model,
            &self.config.provider,
            &self.config.theme,
            self.config.reasoning_effort.as_str(),
        );
        let thread = self
            .thread_manager
            .start_thread_with_identifier(session_id.0.to_string(), ThreadBootstrap::new(Some(metadata)));
        let handle = self.build_session_handle(session_id.clone(), thread, SessionStartTrigger::NewSession);
        if let Ok(mut guard) = self.sessions.lock() {
            drop(guard.insert(session_id.clone(), handle));
        }
        session_id
    }

    fn register_durable_session(
        &self,
        workspace_runtime: Arc<super::SessionWorkspaceRuntime>,
        acp_meta: Option<acp::Meta>,
    ) -> acp::SessionId {
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{}", Uuid::new_v4())));
        let mut metadata = build_thread_archive_metadata(
            workspace_runtime.workspace_root.as_path(),
            &self.config.model,
            &self.config.provider,
            &self.config.theme,
            self.config.reasoning_effort.as_str(),
        )
        .with_primary_agent(workspace_runtime.primary_agents.default_id());
        metadata.acp_meta = Some(serde_json::Map::new());
        let thread = self
            .thread_manager
            .start_thread_with_identifier(session_id.0.to_string(), ThreadBootstrap::new(Some(metadata)));
        let handle = self.build_session_handle_with_archive(
            session_id.clone(),
            thread,
            None,
            SessionStartTrigger::NewSession,
            Some(workspace_runtime),
        );
        self.merge_session_acp_meta(&handle, acp_meta);
        if let Ok(mut guard) = self.sessions.lock() {
            drop(guard.insert(session_id.clone(), handle));
        }
        session_id
    }

    pub(crate) fn session_handle(&self, session_id: &acp::SessionId) -> Option<SessionHandle> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(session_id).cloned()
    }

    pub(super) fn push_message(&self, session: &SessionHandle, message: Message) {
        if let Ok(data) = session.data.lock() {
            data.thread.append_message(message);
        }
    }

    pub(super) async fn checkpoint_session(&self, session: &SessionHandle) -> anyhow::Result<()> {
        if !history_persistence_enabled() {
            debug!("Skipped ACP session checkpoint because history persistence is disabled");
            return Ok(());
        }

        let (existing_archive, snapshot, session_identifier) = {
            let data = session
                .data
                .lock()
                .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
            let snapshot = data.thread.snapshot();
            (data.archive.clone(), snapshot, data.session_id.0.to_string())
        };

        let metadata = snapshot
            .metadata
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ACP session is missing archive metadata"))?;
        let mut archive = match existing_archive {
            Some(archive) => archive,
            None => {
                let archive = SessionArchive::new_with_identifier(metadata.clone(), session_identifier).await?;
                let mut data = session
                    .data
                    .lock()
                    .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
                data.archive = Some(archive.clone());
                archive
            }
        };
        archive.replace_metadata(metadata);

        let messages = snapshot.messages.iter().map(SessionMessage::from).collect::<Vec<_>>();
        let recent_start = messages.len().saturating_sub(20);
        let recent_messages = messages.iter().skip(recent_start).cloned().collect();
        let turn_number = snapshot
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count();
        let status = archive
            .persist_checkpoint_async(SessionProgressArgs {
                total_messages: messages.len(),
                distinct_tools: Vec::new(),
                messages,
                recent_messages,
                turn_number,
                token_usage: None,
                max_context_tokens: None,
                loaded_skills: Some(snapshot.loaded_skills),
            })
            .await?;
        debug!(path = %status.path().display(), ?status, "Processed ACP session checkpoint");
        Ok(())
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(super) fn should_send_tool_notice(&self, session: &SessionHandle) -> bool {
        session
            .data
            .lock()
            .map(|data| !data.tool_notice_sent.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(super) fn mark_tool_notice_sent(&self, session: &SessionHandle) {
        if let Ok(data) = session.data.lock() {
            data.tool_notice_sent.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn update_session_primary_agent(&self, session: &SessionHandle, primary_agent: String) -> bool {
        let workspace_runtime = session.workspace_runtime();
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let Some(primary_agent) = primary_agents.resolve_id(&primary_agent) else {
            return false;
        };
        let mut data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if data.primary_agent.eq_ignore_ascii_case(primary_agent) {
            return false;
        }
        data.primary_agent = primary_agent.to_string();
        self.sync_thread_primary_agent(&data.thread, &data.primary_agent);
        true
    }

    fn update_session_reasoning_effort(&self, session: &SessionHandle, reasoning_effort: ReasoningEffortLevel) -> bool {
        let mut data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if data.reasoning_effort == reasoning_effort {
            return false;
        }
        data.reasoning_effort = reasoning_effort;
        self.sync_thread_reasoning_effort(&data.thread, reasoning_effort);
        true
    }

    fn update_session_provider_and_model(&self, session: &SessionHandle, provider: String, model: String) -> bool {
        let mut data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if data.provider == provider && data.model == model {
            return false;
        }
        data.provider = provider;
        data.model = model;
        self.sync_thread_provider_and_model(&data.thread, &data.provider, &data.model);
        true
    }

    fn provider_default_model(&self, provider: &str) -> Option<String> {
        Provider::from_str(provider)
            .ok()
            .map(|value| ModelId::default_single_for_provider(value).as_str().to_string())
    }

    fn provider_supports_model(&self, provider: &str, model: &str) -> bool {
        let Ok(provider) = Provider::from_str(provider) else {
            return true;
        };
        ModelId::models_for_provider(provider)
            .iter()
            .any(|entry| entry.as_str() == model)
    }

    fn provider_select_options(&self, current_provider: &str) -> Vec<acp::SessionConfigSelectOption> {
        let mut providers = get_factory()
            .lock()
            .ok()
            .map(|factory| factory.list_providers())
            .unwrap_or_default();
        if providers.is_empty() {
            tracing::warn!("LLM factory has no registered providers, falling back to Provider::all_providers()");
            providers = Provider::all_providers()
                .into_iter()
                .map(|provider| provider.to_string())
                .collect();
        }

        if !providers.iter().any(|provider| provider.eq_ignore_ascii_case(current_provider)) {
            providers.push(current_provider.to_string());
        }

        providers.sort();
        providers.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        tracing::debug!(
            provider_count = providers.len(),
            providers = ?providers,
            current_provider = current_provider,
            "Building provider select options for ACP"
        );

        providers
            .into_iter()
            .map(|provider| {
                let name = Provider::from_str(&provider)
                    .ok()
                    .map(|parsed| parsed.label().to_string())
                    .unwrap_or_else(|| provider.clone());
                acp::SessionConfigSelectOption::new(provider, name)
            })
            .collect()
    }

    fn model_select_options(&self, provider: &str, current_model: &str) -> Vec<acp::SessionConfigSelectOption> {
        let mut options = Provider::from_str(provider)
            .ok()
            .map(|provider| {
                ModelId::models_for_provider(provider)
                    .into_iter()
                    .map(|model| {
                        acp::SessionConfigSelectOption::new(
                            model.as_str().into_owned(),
                            model.display_name().into_owned(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if !options.iter().any(|option| option.value.0.as_ref() == current_model) {
            options.push(acp::SessionConfigSelectOption::new(current_model.to_string(), current_model.to_string()));
        }

        options.sort_by(|left, right| left.value.0.cmp(&right.value.0));
        options
    }

    fn supports_provider(&self, provider: &str, current_provider: &str) -> bool {
        self.provider_select_options(current_provider)
            .iter()
            .any(|option| option.value.0.as_ref() == provider)
    }

    fn current_session_config_options(&self, session: &SessionHandle) -> Vec<acp::SessionConfigOption> {
        let data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };
        let provider_options = self.provider_select_options(&data.provider);
        let model_options = self.model_select_options(&data.provider, &data.model);
        let primary_agents = data
            .workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let config_options = session_config_options(
            &data.primary_agent,
            primary_agents,
            data.reasoning_effort,
            self.model_supports_thought_level(&data.provider, &data.model),
            &data.provider,
            provider_options,
            &data.model,
            model_options,
        );

        tracing::debug!(
            config_option_count = config_options.len(),
            primary_agent = %data.primary_agent,
            current_provider = %data.provider,
            current_model = %data.model,
            "Built session config options for ACP"
        );

        config_options
    }

    pub(super) fn resolved_messages(&self, session: &SessionHandle) -> Vec<Message> {
        let workspace_runtime = session.workspace_runtime();
        let system_prompt = workspace_runtime
            .as_ref()
            .map_or(self.system_prompt.as_str(), |runtime| runtime.system_prompt.as_str());
        let mut messages = Vec::with_capacity(10);
        if !system_prompt.trim().is_empty() {
            messages.push(Message::system(system_prompt.to_string()));
        }

        let Ok(history) = session.data.lock() else {
            return messages;
        };
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        if let Some(prompt) = primary_agents.prompt(&history.primary_agent) {
            messages.push(Message::system(prompt.to_string()));
        }
        messages.extend(history.thread.messages());
        messages
    }

    async fn attach_thread_from_archive(
        &self,
        session_id: &acp::SessionId,
        identifier: &str,
        workspace: &std::path::Path,
    ) -> anyhow::Result<SessionHandle> {
        let listing = find_session_by_identifier(identifier)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown archived session '{identifier}'"))?;
        anyhow::ensure!(
            session_listing_matches_workspace(&listing, workspace),
            "archived session '{identifier}' belongs to a different workspace"
        );
        let archive = SessionArchive::resume_from_listing(&listing, listing.snapshot.metadata.clone());
        let thread = self
            .thread_manager
            .start_thread_with_identifier(listing.identifier(), ThreadBootstrap::from_listing(listing));
        let runtime = Arc::new(
            super::SessionWorkspaceRuntime::build(
                &self.config,
                workspace.to_path_buf(),
                &self.workspace_runtime_config,
                self.vt_config.as_deref(),
            )
            .await
            .context("Failed to initialise archived ACP session workspace")?,
        );
        let handle = self.build_session_handle_with_archive(
            session_id.clone(),
            thread,
            Some(archive),
            SessionStartTrigger::Resume,
            Some(runtime),
        );
        if let Ok(mut guard) = self.sessions.lock() {
            drop(guard.insert(session_id.clone(), handle.clone()));
        }
        Ok(handle)
    }

    async fn attach_or_get_session(
        &self,
        session_id: &acp::SessionId,
        workspace: &std::path::Path,
    ) -> anyhow::Result<SessionHandle> {
        if let Some(session) = self.session_handle(session_id) {
            let matches_workspace = session
                .data
                .lock()
                .ok()
                .and_then(|data| data.thread.metadata())
                .is_some_and(|metadata| std::path::Path::new(&metadata.workspace_path) == workspace);
            anyhow::ensure!(matches_workspace, "active session belongs to a different workspace");
            return Ok(session);
        }

        anyhow::ensure!(history_persistence_enabled(), "durable session history is disabled");

        self.attach_thread_from_archive(session_id, session_id.0.as_ref(), workspace)
            .await
    }

    fn session_list_offset(cursor: Option<&str>) -> Result<usize, acp::Error> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        cursor
            .strip_prefix("offset:")
            .and_then(|offset| offset.parse().ok())
            .ok_or_else(|| acp::Error::invalid_params().data("invalid session list cursor"))
    }

    fn session_list_response(
        args: &acp::ListSessionsRequest,
        listings: Vec<SessionListing>,
    ) -> Result<acp::ListSessionsResponse, acp::Error> {
        let offset = Self::session_list_offset(args.cursor.as_deref())?;
        let mut sessions = listings
            .into_iter()
            .filter(|listing| {
                args.cwd
                    .as_deref()
                    .is_none_or(|workspace| session_listing_matches_workspace(listing, workspace))
            })
            .filter_map(|listing| {
                let cwd = session_workspace_path(&listing)?;
                cwd.is_absolute().then(|| {
                    acp::SessionInfo::new(listing.identifier(), cwd)
                        .title(listing.first_prompt_preview().or_else(|| listing.first_reply_preview()))
                        .updated_at(listing.snapshot.ended_at.to_rfc3339())
                })
            })
            .skip(offset)
            .take(Self::SESSION_LIST_PAGE_SIZE + 1)
            .collect::<Vec<_>>();

        let has_more = sessions.len() > Self::SESSION_LIST_PAGE_SIZE;
        sessions.truncate(Self::SESSION_LIST_PAGE_SIZE);
        let next_cursor = has_more.then(|| format!("offset:{}", offset + sessions.len()));
        Ok(acp::ListSessionsResponse::new(sessions).next_cursor(next_cursor))
    }

    /// Programmatic equivalent of the SACP `session/list` handler.
    pub(crate) async fn list_sessions(
        &self,
        args: acp::ListSessionsRequest,
    ) -> Result<acp::ListSessionsResponse, acp::Error> {
        if !history_persistence_enabled() {
            return Ok(acp::ListSessionsResponse::new(Vec::new()));
        }

        let listings = list_recent_sessions(0)
            .await
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        let response = Self::session_list_response(&args, listings)?;
        debug!(
            session_count = response.sessions.len(),
            workspace = ?args.cwd,
            cursor = ?args.cursor,
            has_more = response.next_cursor.is_some(),
            "Listed durable ACP sessions"
        );
        Ok(response)
    }

    pub(super) fn stop_reason_from_finish(finish: FinishReason) -> acp::StopReason {
        match finish {
            FinishReason::Stop | FinishReason::ToolCalls => acp::StopReason::EndTurn,
            FinishReason::Length => acp::StopReason::MaxTokens,
            FinishReason::ContentFilter | FinishReason::Refusal | FinishReason::Error(_) => acp::StopReason::Refusal,
            FinishReason::Pause => acp::StopReason::EndTurn,
        }
    }

    /// Programmatic equivalent of the SACP `session/new` handler — exposed
    /// for tests and for the SACP handler shim to call.
    pub(crate) async fn new_session(&self, req: acp::NewSessionRequest) -> Result<acp::NewSessionResponse, acp::Error> {
        let requested_workspace = req.cwd;
        let workspace = canonical_session_workspace(&requested_workspace).await?;
        let desired_trust = self
            .workspace_runtime_config
            .zed_config
            .workspace_trust
            .to_workspace_trust_level();
        let _trust_outcome = DefaultWorkspaceTrustSynchronizer::new()
            .synchronize(&workspace, desired_trust)
            .await
            .map_err(|error| acp::Error::internal_error().data(format!("Failed to trust ACP session cwd: {error}")))?;
        let workspace_runtime = Arc::new(
            super::SessionWorkspaceRuntime::build(
                &self.config,
                workspace,
                &self.workspace_runtime_config,
                self.vt_config.as_deref(),
            )
            .await
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?,
        );
        let session_id = self.register_durable_session(workspace_runtime, req.meta);
        let session = self.session_handle(&session_id);
        if let Some(session) = &session {
            self.ensure_task_lifecycle_forwarder(session);
            self.run_session_start_hooks(session)
                .await
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        }
        let config_options = session
            .as_ref()
            .map(|session| self.current_session_config_options(session))
            .unwrap_or_default();

        if let Err(error) = self.send_available_commands_update(&session_id).await {
            warn!(%error, "Failed to advertise initial slash commands");
        }

        debug!(%session_id, "Created ACP session");

        Ok(acp::NewSessionResponse::new(session_id).config_options(config_options))
    }

    /// Programmatic equivalent of the SACP `session/load` handler.
    pub(crate) async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        let workspace = canonical_session_workspace(&args.cwd).await?;
        let session = self
            .attach_or_get_session(&args.session_id, &workspace)
            .await
            .map_err(|err| acp::Error::internal_error().data(err.to_string()))?;
        self.ensure_task_lifecycle_forwarder(&session);

        if let Err(error) = self.send_available_commands_update(&args.session_id).await {
            warn!(%error, "Failed to advertise slash commands on session load");
        }

        let config_options = self.current_session_config_options(&session);
        self.run_session_start_hooks(&session)
            .await
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        debug!(session_id = %args.session_id, "Loaded durable ACP session through legacy session/load");
        Ok(acp::LoadSessionResponse::new().config_options(config_options))
    }

    /// Programmatic equivalent of the SACP `session/resume` handler.
    pub(crate) async fn resume_session(
        &self,
        args: acp::ResumeSessionRequest,
    ) -> Result<acp::ResumeSessionResponse, acp::Error> {
        let workspace = canonical_session_workspace(&args.cwd).await?;
        let session = self
            .attach_or_get_session(&args.session_id, &workspace)
            .await
            .map_err(|err| acp::Error::internal_error().data(err.to_string()))?;
        self.ensure_task_lifecycle_forwarder(&session);

        if let Err(error) = self.send_available_commands_update(&args.session_id).await {
            warn!(%error, "Failed to advertise slash commands on session resume");
        }

        let config_options = self.current_session_config_options(&session);
        self.run_session_start_hooks(&session)
            .await
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        debug!(session_id = %args.session_id, "Resumed durable ACP session");
        Ok(acp::ResumeSessionResponse::new().config_options(config_options))
    }

    /// Programmatic equivalent of the SACP `session/set_config_option`
    /// handler. Used both by the SACP handler and by the test suite.
    pub(crate) async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> Result<acp::SetSessionConfigOptionResponse, acp::Error> {
        use crate::zed::helpers::SESSION_CONFIG_MODEL_ID;
        use crate::zed::helpers::SESSION_CONFIG_PRIMARY_AGENT_ID;
        use crate::zed::helpers::SESSION_CONFIG_PROVIDER_ID;
        use crate::zed::helpers::SESSION_CONFIG_THOUGHT_LEVEL_ID;

        let Some(session) = self.session_handle(&args.session_id) else {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_session"
            })));
        };

        let config_id = args.config_id.0.to_string();
        let value = match &args.value {
            acp::SessionConfigOptionValue::ValueId { value } => value.0.as_ref().to_string(),
            _ => {
                return Err(acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "expected_string_config_option_value",
                    "config_id": config_id,
                })));
            }
        };
        let updated = match config_id.as_str() {
            SESSION_CONFIG_PRIMARY_AGENT_ID => {
                let workspace_runtime = session.workspace_runtime();
                let primary_agents = workspace_runtime
                    .as_ref()
                    .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
                let Some(primary_agent) = primary_agents.resolve_id(&value) else {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_primary_agent",
                        "value": value,
                    })));
                };
                self.update_session_primary_agent(&session, primary_agent.to_string())
            }
            SESSION_CONFIG_THOUGHT_LEVEL_ID => {
                let (session_provider, session_model) = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    (data.provider.clone(), data.model.clone())
                };
                if !self.model_supports_thought_level(&session_provider, &session_model) {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unsupported_config_option",
                        "config_id": config_id,
                    })));
                }
                let Some(reasoning_effort) = ReasoningEffortLevel::parse(&value) else {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_thought_level",
                        "value": value,
                    })));
                };
                self.update_session_reasoning_effort(&session, reasoning_effort)
            }
            SESSION_CONFIG_PROVIDER_ID => {
                let provider = value.trim().to_lowercase();
                let current_provider = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    data.provider.clone()
                };
                if provider.is_empty() || !self.supports_provider(&provider, &current_provider) {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_provider",
                        "value": value,
                    })));
                }
                let current_model = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    data.model.clone()
                };
                let resolved_model = if self.provider_supports_model(&provider, &current_model) {
                    current_model
                } else {
                    self.provider_default_model(&provider).ok_or_else(|| {
                        acp::Error::invalid_params().data(serde_json::json!({
                            "reason": "provider_has_no_default_model",
                            "provider": provider,
                        }))
                    })?
                };
                self.update_session_provider_and_model(&session, provider, resolved_model)
            }
            SESSION_CONFIG_MODEL_ID => {
                let model = value.trim();
                if model.is_empty() {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_model",
                        "value": value,
                    })));
                }
                let provider = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    data.provider.clone()
                };
                if !self.provider_supports_model(&provider, model) {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "model_not_supported_for_provider",
                        "provider": provider,
                        "model": model,
                    })));
                }
                self.update_session_provider_and_model(&session, provider, model.to_string())
            }
            _ => {
                return Err(acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "unknown_config_option",
                    "config_id": config_id,
                })));
            }
        };

        if updated {
            let config_options = self.current_session_config_options(&session);
            let update = acp::ConfigOptionUpdate::new(config_options);
            drop(
                self.send_update(&args.session_id, acp::SessionUpdate::ConfigOptionUpdate(update))
                    .await,
            );
        }

        let config_options = self.current_session_config_options(&session);
        Ok(acp::SetSessionConfigOptionResponse::new(config_options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::helpers::PrimaryAgentCatalog;
    use assert_fs::TempDir;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::sync::LazyLock;
    use tokio::sync::Mutex as AsyncMutex;
    use vtcode_config::codex::HistoryPersistence;
    use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
    use vtcode_core::config::core::PromptCachingConfig;
    use vtcode_core::config::types::{AgentConfig as CoreAgentConfig, ModelSelectionSource, UiSurfacePreference};
    use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
    use vtcode_core::core::agent::snapshots::{
        DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS,
    };
    use vtcode_core::utils::session_archive::{SessionArchiveMetadata, SessionListing, SessionSnapshot};

    static HISTORY_TEST_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

    struct HistorySettingsGuard;

    impl HistorySettingsGuard {
        fn set(persistence: HistoryPersistence, max_bytes: Option<usize>) -> Self {
            let mut config = vtcode_core::config::VTCodeConfig::default();
            config.history.persistence = persistence;
            config.history.max_bytes = max_bytes;
            vtcode_core::utils::session_archive::apply_session_history_config_from_vtcode(&config);
            Self
        }
    }

    impl Drop for HistorySettingsGuard {
        fn drop(&mut self) {
            vtcode_core::utils::session_archive::apply_session_history_config_from_vtcode(
                &vtcode_core::config::VTCodeConfig::default(),
            );
        }
    }

    async fn build_agent(workspace: &Path) -> ZedAgent {
        build_agent_with_default_primary_agent(workspace, "duck").await
    }

    async fn build_agent_with_default_primary_agent(workspace: &Path, default_primary_agent: &str) -> ZedAgent {
        let core_config = CoreAgentConfig {
            model: "gpt-5.6-sol".to_string(),
            api_key: String::new(),
            provider: "openai".to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            workspace: workspace.to_path_buf(),
            verbose: false,
            quiet: false,
            theme: "test".to_string(),
            reasoning_effort: ReasoningEffortLevel::Low,
            ui_surface: UiSurfacePreference::default(),
            prompt_cache: PromptCachingConfig::default(),
            model_source: ModelSelectionSource::WorkspaceConfig,
            custom_api_keys: BTreeMap::new(),
            checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
            checkpointing_storage_dir: None,
            checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
            checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
            max_conversation_turns: 1000,
            model_behavior: None,
            openai_chatgpt_auth: None,
        };

        let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
        discovery_input.include_user_agents = false;
        let discovered = discover_subagents(&discovery_input).expect("discover primary agents");
        let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent);

        Box::pin(ZedAgent::new(
            core_config,
            AuthCredentialsStoreMode::default(),
            AgentClientProtocolZedConfig::default(),
            ToolsConfig::default(),
            CommandsConfig::default(),
            &[],
            vtcode_config::TimeoutsConfig::default(),
            String::new(),
            Some("Zed".to_string()),
            primary_agents,
            false,
            None,
            None,
        ))
        .await
    }

    fn primary_agent(session: &SessionHandle) -> String {
        session.data.lock().map(|data| data.primary_agent.clone()).unwrap_or_default()
    }

    fn reasoning_effort(session: &SessionHandle) -> ReasoningEffortLevel {
        session
            .data
            .lock()
            .map(|data| data.reasoning_effort)
            .unwrap_or(ReasoningEffortLevel::Low)
    }

    fn provider(session: &SessionHandle) -> String {
        session.data.lock().map(|data| data.provider.clone()).unwrap_or_default()
    }

    fn model(session: &SessionHandle) -> String {
        session.data.lock().map(|data| data.model.clone()).unwrap_or_default()
    }

    #[tokio::test]
    async fn build_session_handle_restores_session_config_from_thread_metadata() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let listing = SessionListing {
            path: temp.path().join("session-vtcode-acp-archive.json"),
            snapshot: SessionSnapshot {
                metadata: SessionArchiveMetadata::new(
                    "vtcode",
                    temp.path().to_string_lossy(),
                    "gpt-5.6-sol",
                    "openai",
                    "test",
                    "xhigh",
                )
                .with_primary_agent("build"),
                started_at: Utc::now(),
                ended_at: Utc::now(),
                total_messages: 0,
                distinct_tools: Vec::new(),
                transcript: Vec::new(),
                messages: Vec::new(),
                progress: None,
                error_logs: Vec::new(),
            },
        };
        let thread = agent
            .thread_manager
            .start_thread_with_identifier("session-vtcode-acp-archive", ThreadBootstrap::from_listing(listing));

        let handle =
            agent.build_session_handle(acp::SessionId::new("session-1"), thread, SessionStartTrigger::NewSession);

        assert_eq!(primary_agent(&handle), "build");
        assert_eq!(reasoning_effort(&handle), ReasoningEffortLevel::XHigh);
        assert_eq!(provider(&handle), "openai");
        assert_eq!(model(&handle), "gpt-5.6-sol");
    }

    #[tokio::test]
    async fn checkpoint_session_persists_messages_for_a_fresh_runtime() {
        let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
        let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let path = temp.path().join("vtcode-zed-session-resume.json");
        let metadata =
            SessionArchiveMetadata::new("vtcode", temp.path().to_string_lossy(), "gpt-5.4", "openai", "test", "low")
                .with_primary_agent("build");
        let listing = SessionListing {
            path: path.clone(),
            snapshot: SessionSnapshot {
                metadata: metadata.clone(),
                started_at: Utc::now(),
                ended_at: Utc::now(),
                total_messages: 0,
                distinct_tools: Vec::new(),
                transcript: Vec::new(),
                messages: Vec::new(),
                progress: None,
                error_logs: Vec::new(),
            },
        };
        let archive = SessionArchive::resume_from_listing(&listing, metadata);
        let thread = agent
            .thread_manager
            .start_thread_with_identifier("vtcode-zed-session-resume", ThreadBootstrap::from_listing(listing));
        let handle = agent.build_session_handle_with_archive(
            acp::SessionId::new("vtcode-zed-session-resume"),
            thread,
            Some(archive),
            SessionStartTrigger::Resume,
            None,
        );
        agent.push_message(&handle, Message::user("continue".to_string()));
        agent.push_message(&handle, Message::assistant("resumed response".to_string()));

        agent.checkpoint_session(&handle).await.unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let snapshot: SessionSnapshot = serde_json::from_str(&raw).unwrap();
        let reloaded = SessionListing { path, snapshot };
        let fresh_thread = agent
            .thread_manager
            .start_thread_with_identifier("vtcode-zed-session-fresh-runtime", ThreadBootstrap::from_listing(reloaded));
        assert_eq!(fresh_thread.messages().len(), 2);
        assert_eq!(fresh_thread.messages()[0].content.as_text(), "continue");
        assert_eq!(fresh_thread.messages()[1].content.as_text(), "resumed response");
    }

    fn archived_listing(identifier: &str, workspace: &Path, prompt: &str) -> SessionListing {
        let now = Utc::now();
        SessionListing {
            path: workspace.join(format!("{identifier}.json")),
            snapshot: SessionSnapshot {
                metadata: SessionArchiveMetadata::new(
                    "workspace",
                    workspace.to_string_lossy(),
                    "gpt-5.4",
                    "openai",
                    "test",
                    "low",
                ),
                started_at: now,
                ended_at: now,
                total_messages: 1,
                distinct_tools: Vec::new(),
                transcript: Vec::new(),
                messages: vec![SessionMessage::new(MessageRole::User, prompt)],
                progress: None,
                error_logs: Vec::new(),
            },
        }
    }

    #[test]
    fn session_list_filters_archives_by_workspace() {
        let first_workspace = Path::new("/workspace/first");
        let second_workspace = Path::new("/workspace/second");
        let request = acp::ListSessionsRequest::new().cwd(first_workspace);

        let response = ZedAgent::session_list_response(
            &request,
            vec![
                archived_listing("session-first", first_workspace, "First task"),
                archived_listing("session-second", second_workspace, "Second task"),
            ],
        )
        .unwrap();

        assert_eq!(response.sessions.len(), 1);
        assert_eq!(response.sessions[0].session_id, acp::SessionId::new("session-first"));
        assert_eq!(response.sessions[0].cwd, first_workspace);
        assert_eq!(response.sessions[0].title.as_deref(), Some("First task"));
        assert!(response.next_cursor.is_none());
    }

    #[test]
    fn session_list_rejects_unknown_cursor() {
        let request = acp::ListSessionsRequest::new().cursor("not-a-vtcode-cursor");
        assert!(ZedAgent::session_list_response(&request, Vec::new()).is_err());
    }

    #[test]
    fn session_list_cursor_advances_to_the_next_page() {
        let workspace = Path::new("/workspace/page");
        let listings = (0..=ZedAgent::SESSION_LIST_PAGE_SIZE)
            .map(|index| archived_listing(&format!("session-{index}"), workspace, "task"))
            .collect::<Vec<_>>();

        let first = ZedAgent::session_list_response(&acp::ListSessionsRequest::new(), listings.clone()).unwrap();
        assert_eq!(first.sessions.len(), ZedAgent::SESSION_LIST_PAGE_SIZE);
        let cursor = first.next_cursor.expect("first page should provide a cursor");

        let second =
            ZedAgent::session_list_response(&acp::ListSessionsRequest::new().cursor(cursor), listings).unwrap();
        assert_eq!(second.sessions.len(), 1);
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn session_metadata_captures_and_merges_acp_meta() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let creation_meta = acp::Meta::from_iter([
            ("client".to_string(), serde_json::json!("zed")),
            ("requestId".to_string(), serde_json::json!("create-1")),
        ]);

        let response = agent
            .new_session(acp::NewSessionRequest::new(temp.path()).meta(creation_meta))
            .await
            .unwrap();
        let session = agent.session_handle(&response.session_id).unwrap();
        agent.merge_session_acp_meta(
            &session,
            Some(acp::Meta::from_iter([
                ("requestId".to_string(), serde_json::json!("prompt-1")),
                ("traceparent".to_string(), serde_json::json!("00-test-trace")),
            ])),
        );

        let metadata = session.data.lock().unwrap().thread.metadata().unwrap();
        let serialized = serde_json::to_value(metadata).unwrap();
        assert_eq!(serialized["acp_meta"]["client"], "zed");
        assert_eq!(serialized["acp_meta"]["requestId"], "prompt-1");
        assert_eq!(serialized["acp_meta"]["traceparent"], "00-test-trace");
    }

    #[tokio::test]
    async fn session_metadata_records_absent_acp_meta_as_an_empty_object() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;

        let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
        let session = agent.session_handle(&response.session_id).unwrap();
        let metadata = session.data.lock().unwrap().thread.metadata().unwrap();
        let serialized = serde_json::to_value(metadata).unwrap();

        assert_eq!(serialized["acp_meta"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn new_session_canonicalizes_and_isolates_requested_workspaces() {
        let launch_workspace = TempDir::new().unwrap();
        let first_workspace = TempDir::new().unwrap();
        let second_workspace = TempDir::new().unwrap();
        let agent = build_agent(launch_workspace.path()).await;

        let (first, second) = tokio::join!(
            agent.new_session(acp::NewSessionRequest::new(first_workspace.path())),
            agent.new_session(acp::NewSessionRequest::new(second_workspace.path()))
        );
        let first = first.unwrap();
        let second = second.unwrap();

        let first_session = agent.session_handle(&first.session_id).unwrap();
        let second_session = agent.session_handle(&second.session_id).unwrap();
        let first_root = first_session.workspace_runtime().unwrap().workspace_root.clone();
        let second_root = second_session.workspace_runtime().unwrap().workspace_root.clone();
        assert_eq!(first_root, vtcode_commons::canonicalize(first_workspace.path()).unwrap());
        assert_eq!(second_root, vtcode_commons::canonicalize(second_workspace.path()).unwrap());
        assert_ne!(first_root, second_root);
        assert_ne!(first_root, vtcode_commons::canonicalize(launch_workspace.path()).unwrap());

        let first_metadata = first_session.data.lock().unwrap().thread.metadata().unwrap();
        let second_metadata = second_session.data.lock().unwrap().thread.metadata().unwrap();
        assert_eq!(Path::new(&first_metadata.workspace_path), first_root);
        assert_eq!(Path::new(&second_metadata.workspace_path), second_root);
    }

    #[tokio::test]
    async fn new_session_rejects_invalid_workspaces_without_registering_a_session() {
        let launch_workspace = TempDir::new().unwrap();
        let agent = build_agent(launch_workspace.path()).await;
        let missing = launch_workspace.path().join("missing");

        let error = agent
            .new_session(acp::NewSessionRequest::new(missing))
            .await
            .expect_err("missing cwd must be rejected");

        assert_eq!(error.code, acp::ErrorCode::InvalidParams);
        assert!(agent.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn checkpoint_session_skips_fresh_archive_when_history_persistence_is_disabled() {
        let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
        for persistence in [HistoryPersistence::None, HistoryPersistence::Unknown] {
            let _history_settings = HistorySettingsGuard::set(persistence, None);
            let temp = TempDir::new().unwrap();
            let agent = build_agent(temp.path()).await;
            let session_id = agent
                .new_session(acp::NewSessionRequest::new(temp.path()))
                .await
                .unwrap()
                .session_id;
            let session = agent.session_handle(&session_id).unwrap();
            agent.push_message(&session, Message::user("do not persist".to_string()));

            agent.checkpoint_session(&session).await.unwrap();

            assert!(session.data.lock().unwrap().archive.is_none());
            assert!(find_session_by_identifier(&session_id.0).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn resume_archive_is_disabled_when_history_persistence_is_disabled() {
        let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
        let _history_settings = HistorySettingsGuard::set(HistoryPersistence::None, None);
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;

        let result = agent
            .resume_session(acp::ResumeSessionRequest::new("archived-session", temp.path()))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn build_session_handle_falls_back_for_unknown_metadata_primary_agent() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let listing = SessionListing {
            path: temp.path().join("session-vtcode-acp-archive.json"),
            snapshot: SessionSnapshot {
                metadata: SessionArchiveMetadata::new(
                    "vtcode",
                    temp.path().to_string_lossy(),
                    "gpt-5.6-sol",
                    "openai",
                    "test",
                    "low",
                )
                .with_primary_agent("missing"),
                started_at: Utc::now(),
                ended_at: Utc::now(),
                total_messages: 0,
                distinct_tools: Vec::new(),
                transcript: Vec::new(),
                messages: Vec::new(),
                progress: None,
                error_logs: Vec::new(),
            },
        };
        let thread = agent
            .thread_manager
            .start_thread_with_identifier("session-vtcode-acp-archive", ThreadBootstrap::from_listing(listing));

        let handle =
            agent.build_session_handle(acp::SessionId::new("session-1"), thread, SessionStartTrigger::NewSession);

        assert_eq!(primary_agent(&handle), "duck");
    }

    #[tokio::test]
    async fn register_session_falls_back_for_unknown_default_primary_agent() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent_with_default_primary_agent(temp.path(), "research").await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        // "research" is not in the discovered specs, so the resolver falls
        // back to the built-in "build" agent.
        assert_eq!(primary_agent(&session), "build");
    }

    #[tokio::test]
    async fn register_session_uses_known_default_primary_agent_ids() {
        for primary_agent in ["duck", "plan", "build", "auto"] {
            let temp = TempDir::new().unwrap();
            let agent = build_agent_with_default_primary_agent(temp.path(), primary_agent).await;
            let session_id = agent.register_session();
            let session = agent.session_handle(&session_id).unwrap();

            assert_eq!(self::primary_agent(&session), primary_agent);
        }
    }

    #[tokio::test]
    async fn register_session_uses_custom_default_primary_agent() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
        fs::write(
            temp.path().join(".vtcode/agents/research.md"),
            r#"---
name: research
description: Research primary
mode: primary
permissions:
  default: deny
---
Research primary prompt."#,
        )
        .unwrap();

        let agent = build_agent_with_default_primary_agent(temp.path(), "research").await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        assert_eq!(primary_agent(&session), "research");
    }

    #[tokio::test]
    async fn update_session_primary_agent_updates_session_data() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        assert!(agent.update_session_primary_agent(&session, "build".to_string()));
        assert_eq!(primary_agent(&session), "build");
        assert_eq!(
            session
                .data
                .lock()
                .ok()
                .and_then(|data| data.thread.metadata())
                .and_then(|metadata| metadata.primary_agent)
                .as_deref(),
            Some("build")
        );
    }

    #[tokio::test]
    async fn update_session_reasoning_effort_syncs_thread_metadata() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        assert!(agent.update_session_reasoning_effort(&session, ReasoningEffortLevel::High));
        assert_eq!(
            session
                .data
                .lock()
                .ok()
                .and_then(|data| data.thread.metadata())
                .map(|metadata| metadata.reasoning_effort)
                .as_deref(),
            Some("high")
        );
    }

    #[tokio::test]
    async fn current_session_config_options_omit_thought_level_when_model_lacks_support() {
        let temp = TempDir::new().unwrap();
        let mut agent = build_agent(temp.path()).await;
        agent.config.model = "claude-haiku-3".to_string();
        agent.config.provider = "anthropic".to_string();
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        let config_options = agent.current_session_config_options(&session);

        assert_eq!(config_options.len(), 3);
        assert_eq!(config_options[0].id, acp::SessionConfigId::new("primary_agent"));
    }
}
