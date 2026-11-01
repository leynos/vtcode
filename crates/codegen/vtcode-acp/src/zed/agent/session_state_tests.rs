//! Builds ACP agent fixtures and groups tests for session state, configuration, metadata, and workspaces.
use super::super::ZedAgentSetup;
use super::*;
use crate::zed::AcpWorkspacePolicy;
use crate::zed::helpers::PrimaryAgentCatalog;
use chrono::Utc;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vtcode_config::codex::HistoryPersistence;
use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::utils::session_archive::{
    SessionArchiveMetadata, SessionListing, SessionMessage, SessionSnapshot, find_session_by_identifier,
};

struct NoOpWorkspaceTrustSynchronizer;

#[async_trait::async_trait]
impl WorkspaceTrustSynchronizer for NoOpWorkspaceTrustSynchronizer {
    async fn synchronize(
        &self,
        _workspace: &Path,
        desired_level: vtcode_core::config::WorkspaceTrustLevel,
    ) -> anyhow::Result<WorkspaceTrustSyncOutcome> {
        Ok(WorkspaceTrustSyncOutcome::AlreadyMatches(desired_level))
    }
}

use crate::zed::agent::test_support::HistorySettingsGuard;
use crate::zed::agent::test_support::SESSION_HISTORY_TEST_LOCK as HISTORY_TEST_LOCK;
#[derive(Debug, PartialEq)]
struct SessionConfigProjection {
    primary_agent: String,
    reasoning_effort: ReasoningEffortLevel,
    provider: String,
    model: String,
}

fn session_config_projection(session: &SessionHandle) -> SessionConfigProjection {
    SessionConfigProjection {
        primary_agent: primary_agent(session),
        reasoning_effort: reasoning_effort(session),
        provider: provider(session),
        model: model(session),
    }
}

async fn build_agent(workspace: &Path) -> ZedAgent {
    build_agent_with_default_primary_agent(workspace, "duck").await
}

async fn build_agent_with_default_primary_agent(workspace: &Path, default_primary_agent: &str) -> ZedAgent {
    build_agent_with_vt_config(workspace, default_primary_agent, None).await
}

async fn build_agent_with_vt_config(
    workspace: &Path,
    default_primary_agent: &str,
    vt_config: Option<&vtcode_core::config::VTCodeConfig>,
) -> ZedAgent {
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
        model_behaviour: None,
        openai_chatgpt_auth: None,
    };

    let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
    discovery_input.include_user_agents = false;
    let discovered = discover_subagents(&discovery_input).expect("discover primary agents");
    let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent);

    let mut zed_config = AgentClientProtocolZedConfig::default();
    zed_config.allowed_workspace_roots.push(std::env::temp_dir());

    let mut agent = Box::pin(ZedAgent::new(ZedAgentSetup {
        config: core_config,
        credential_storage_mode: AuthCredentialsStoreMode::default(),
        zed_config,
        tools_config: ToolsConfig::default(),
        commands_config: CommandsConfig::default(),
        custom_providers: &[],
        provider_timeouts: vtcode_config::TimeoutsConfig::default(),
        system_prompt: String::new(),
        title: Some("Zed".to_string()),
        primary_agents,
        skip_confirmations: false,
        vt_config,
        audit_logger: None,
    }))
    .await
    .expect("build test ACP agent");
    agent.workspace_trust_synchronizer = Arc::new(NoOpWorkspaceTrustSynchronizer);
    agent
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

fn session_listing(
    identifier: &str,
    workspace: &Path,
    metadata: SessionArchiveMetadata,
    messages: Vec<SessionMessage>,
) -> SessionListing {
    let now = Utc::now();
    let total_messages = messages.len();
    SessionListing {
        path: workspace.join(format!("{identifier}.json")),
        snapshot: SessionSnapshot {
            metadata,
            started_at: now,
            ended_at: now,
            total_messages,
            distinct_tools: Vec::new(),
            transcript: Vec::new(),
            messages,
            progress: None,
            error_logs: Vec::new(),
        },
    }
}

#[path = "session_state_tests/archive.rs"]
mod archive;
#[path = "session_state_tests/configuration.rs"]
mod configuration;
#[path = "session_state_tests/metadata.rs"]
mod metadata;
#[path = "session_state_tests/tool_recovery.rs"]
mod tool_recovery;
#[path = "session_state_tests/workspace.rs"]
mod workspace;
