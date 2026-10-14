//! Shared setup helpers for Zed ACP tool configuration tests.
use super::super::{ZedAgent, ZedAgentSetup};
use crate::zed::helpers::PrimaryAgentCatalog;
use std::collections::BTreeMap;
use std::path::Path;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::llm::provider::ToolDefinition;

pub(super) async fn build_agent(workspace: &Path) -> ZedAgent {
    build_agent_with_tools_config(workspace, ToolsConfig::default()).await
}

pub(super) async fn build_agent_with_tools_config(workspace: &Path, tools_config: ToolsConfig) -> ZedAgent {
    build_agent_with_config(workspace, tools_config, None).await
}

pub(super) async fn build_agent_with_config(
    workspace: &Path,
    tools_config: ToolsConfig,
    vt_cfg: Option<&VTCodeConfig>,
) -> ZedAgent {
    let core_config = core_agent_config(workspace);

    let mut zed_config = AgentClientProtocolZedConfig::default();
    zed_config.tools.list_files = true;
    zed_config.tools.read_file = false;

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
        provider_timeouts: vtcode_config::TimeoutsConfig::default(),
        system_prompt: String::new(),
        title: Some("Zed".to_string()),
        primary_agents,
        skip_confirmations: false,
        vt_config: vt_cfg,
        audit_logger: None,
    }))
    .await
}

pub(super) fn core_agent_config(workspace: &Path) -> CoreAgentConfig {
    CoreAgentConfig {
        model: "test-model".to_string(),
        api_key: String::new(),
        provider: "test-provider".to_string(),
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
    }
}

pub(super) fn definition_names(definitions: Vec<ToolDefinition>) -> Vec<String> {
    definitions
        .into_iter()
        .map(|definition| definition.function_name().to_string())
        .collect()
}
