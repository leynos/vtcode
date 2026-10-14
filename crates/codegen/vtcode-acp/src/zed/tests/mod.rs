//! Shared fixtures and helpers for the ACP Zed agent test modules.

use super::agent::{ZedAgent, ZedAgentSetup};
use super::*;
use crate::tooling::{TOOL_LIST_FILES_ITEMS_KEY, TOOL_LIST_FILES_RESULT_KEY, TOOL_LIST_FILES_URI_ARG};
use crate::zed::helpers::{
    PrimaryAgentCatalog, SESSION_CONFIG_MODEL_ID, SESSION_CONFIG_PRIMARY_AGENT_ID, SESSION_CONFIG_PROVIDER_ID,
    SESSION_CONFIG_THOUGHT_LEVEL_ID,
};
use agent_client_protocol::schema::v1::{
    LoadSessionRequest, NewSessionRequest, ResumeSessionRequest, SetSessionConfigOptionRequest, ToolCallStatus,
};
use agent_client_protocol::schema::v1::{SessionConfigKind, SessionConfigSelectOptions};
use assert_fs::TempDir;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::fs;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::hooks::{HookCommandConfig, HookGroupConfig};
use vtcode_config::{SubagentDiscoveryInput, TimeoutsConfig, discover_subagents};
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::models::{ModelId, Provider};
use vtcode_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};

async fn build_agent(workspace: &Path) -> ZedAgent {
    build_agent_with_vt_config(workspace, None).await
}

async fn build_agent_with_vt_config(workspace: &Path, vt_cfg: Option<&VTCodeConfig>) -> ZedAgent {
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

    let mut zed_config = AgentClientProtocolZedConfig::default();
    zed_config.tools.list_files = true;
    zed_config.tools.read_file = false;

    let tools_config = ToolsConfig::default();
    let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
    discovery_input.include_user_agents = false;
    let discovered = discover_subagents(&discovery_input).expect("discover primary agents");
    let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, "duck");

    Box::pin(ZedAgent::new(ZedAgentSetup {
        config: core_config,
        credential_storage_mode: AuthCredentialsStoreMode::default(),
        zed_config,
        tools_config,
        commands_config: CommandsConfig::default(),
        custom_providers: &[],
        provider_timeouts: TimeoutsConfig::default(),
        system_prompt: String::new(),
        title: Some("Zed".to_string()),
        primary_agents,
        skip_confirmations: false,
        vt_config: vt_cfg,
        audit_logger: None,
    }))
    .await
}

mod session_config;
mod sessions;
mod tools;
