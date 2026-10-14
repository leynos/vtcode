//! Workspace-scoped ACP registries, prompts, permissions, and runtime setup.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::warn;
use vtcode_config::core::CustomProviderConfig;
use vtcode_config::{SubagentDiscoveryInput, VTCodeConfig, discover_subagents};
use vtcode_core::config::ToolDocumentationMode;
use vtcode_core::config::types::{AgentConfig as CoreAgentConfig, CapabilityLevel};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
use vtcode_core::prompts::system::generate_system_instruction_with_config;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::grep_file::GrepSearchManager;
use vtcode_core::tools::handlers::{SessionSurface, SessionToolsConfig, ToolModelCapabilities};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

use super::super::helpers::PrimaryAgentCatalog;
use super::super::session::AcpSessionWorkspace;
use super::{
    AcpSubagentControllerSetup, ZedAgent, ZedAgentSetup, attach_acp_mcp_client, attach_acp_subagent_controller,
    configure_acp_tool_call_limits, report_optional_setup_outcome,
};
use crate::permissions::{AcpPermissionPrompter, DefaultPermissionPrompter};
use crate::tooling::AcpToolRegistry;

pub(crate) struct SessionWorkspaceRuntime {
    pub(super) workspace_root: PathBuf,
    pub(super) workspace_hooks_gated: bool,
    pub(super) system_prompt: String,
    pub(super) primary_agents: PrimaryAgentCatalog,
    pub(super) acp_tool_registry: Arc<AcpToolRegistry>,
    pub(super) permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
    pub(super) local_tool_registry: CoreToolRegistry,
    pub(super) file_ops_tool: Option<FileOpsTool>,
}

#[derive(Clone)]
pub(super) struct SessionWorkspaceRuntimeConfig {
    pub(super) zed_config: AgentClientProtocolZedConfig,
    pub(super) tools_config: ToolsConfig,
    pub(super) commands_config: CommandsConfig,
    pub(super) custom_providers: Vec<CustomProviderConfig>,
    pub(super) skip_confirmations: bool,
}

struct SessionWorkspaceRuntimeSetup<'a> {
    base_config: &'a CoreAgentConfig,
    workspace: &'a AcpSessionWorkspace,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
    vt_config: Option<&'a VTCodeConfig>,
}

struct PreparedSessionWorkspace<'a> {
    workspace_root: PathBuf,
    session_config: CoreAgentConfig,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
    vt_config: Option<&'a VTCodeConfig>,
    system_prompt: String,
    primary_agents: PrimaryAgentCatalog,
    workspace_hooks_gated: bool,
    file_ops_tool: Option<FileOpsTool>,
}

struct AcpWorkspaceToolSurface {
    registry: Arc<AcpToolRegistry>,
    permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
}

struct OptionalWorkspaceComponentsSetup<'a> {
    registry: &'a CoreToolRegistry,
    agent_config: &'a CoreAgentConfig,
    custom_providers: &'a [CustomProviderConfig],
    vt_config: Option<&'a VTCodeConfig>,
    workspace_gated: bool,
    workspace_root: &'a Path,
}

pub(super) async fn build_launch_workspace_runtime(setup: &ZedAgentSetup<'_>) -> SessionWorkspaceRuntime {
    let workspace_root = setup.config.workspace.clone();
    let file_ops_tool = create_file_ops_tool(&workspace_root, &setup.zed_config);
    let local_tool_registry = CoreToolRegistry::new(workspace_root.clone()).await;
    configure_acp_tool_call_limits(&local_tool_registry, setup.vt_config);
    if let Err(error) = local_tool_registry
        .apply_tool_runtime_config(&setup.commands_config, &setup.tools_config)
        .await
    {
        warn!(%error, "Failed to apply tools configuration to ACP tool registry");
    }

    let workspace_hooks_gated = setup
        .vt_config
        .and_then(|config| config.workspace_lifecycle_hooks.as_ref())
        .is_some_and(|hooks| !hooks.is_empty());
    attach_optional_workspace_components(OptionalWorkspaceComponentsSetup {
        registry: &local_tool_registry,
        agent_config: &setup.config,
        custom_providers: setup.custom_providers,
        vt_config: setup.vt_config,
        workspace_gated: workspace_hooks_gated,
        workspace_root: &workspace_root,
    })
    .await;

    let tool_surface = build_acp_tool_surface(
        &local_tool_registry,
        &workspace_root,
        &setup.zed_config,
        &setup.tools_config,
        setup.skip_confirmations,
        file_ops_tool.is_some(),
    )
    .await;

    SessionWorkspaceRuntime {
        workspace_root,
        workspace_hooks_gated,
        system_prompt: setup.system_prompt.clone(),
        primary_agents: setup.primary_agents.clone(),
        acp_tool_registry: tool_surface.registry,
        permission_prompter: tool_surface.permission_prompter,
        local_tool_registry,
        file_ops_tool,
    }
}

impl SessionWorkspaceRuntime {
    async fn build(setup: SessionWorkspaceRuntimeSetup<'_>) -> anyhow::Result<Self> {
        let prepared = prepare_session_workspace(setup).await?;
        let local_tool_registry = initialize_session_tool_registry(&prepared).await?;
        let tool_surface = build_acp_tool_surface(
            &local_tool_registry,
            &prepared.workspace_root,
            &prepared.runtime_config.zed_config,
            &prepared.runtime_config.tools_config,
            prepared.runtime_config.skip_confirmations,
            prepared.file_ops_tool.is_some(),
        )
        .await;

        Ok(Self {
            workspace_root: prepared.workspace_root,
            workspace_hooks_gated: prepared.workspace_hooks_gated,
            system_prompt: prepared.system_prompt,
            primary_agents: prepared.primary_agents,
            acp_tool_registry: tool_surface.registry,
            permission_prompter: tool_surface.permission_prompter,
            local_tool_registry,
            file_ops_tool: prepared.file_ops_tool,
        })
    }
}

impl ZedAgent {
    pub(super) async fn workspace_runtime_for(
        &self,
        workspace: &AcpSessionWorkspace,
    ) -> anyhow::Result<Arc<SessionWorkspaceRuntime>> {
        let workspace_root = workspace.path().to_path_buf();
        let runtime_cell = {
            let mut runtimes = self.workspace_runtimes.lock().await;
            Arc::clone(
                runtimes
                    .entry(workspace_root.clone())
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            )
        };
        let runtime = runtime_cell
            .get_or_try_init(|| async {
                SessionWorkspaceRuntime::build(SessionWorkspaceRuntimeSetup {
                    base_config: &self.config,
                    workspace,
                    runtime_config: &self.workspace_runtime_config,
                    vt_config: self.vt_config.as_deref(),
                })
                .await
                .map(Arc::new)
            })
            .await?;
        Ok(Arc::clone(runtime))
    }
}

async fn prepare_session_workspace<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
) -> anyhow::Result<PreparedSessionWorkspace<'a>> {
    let SessionWorkspaceRuntimeSetup { base_config, workspace, runtime_config, vt_config } = setup;
    let workspace_root = workspace.path().to_path_buf();
    let mut session_config = base_config.clone();
    session_config.workspace = workspace_root.clone();
    let content = generate_system_instruction_with_config(&Default::default(), &workspace_root, vt_config).await;
    let system_prompt = content
        .parts
        .first()
        .and_then(|part| part.as_text())
        .map_or_else(String::new, ToString::to_string);
    let discovered = discover_subagents(&SubagentDiscoveryInput::new(workspace_root.clone()))?;
    let default_primary_agent = vt_config.map_or("duck", |config| config.default_primary_agent.as_str());
    let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent);
    let workspace_hooks_gated = vt_config
        .and_then(|config| config.workspace_lifecycle_hooks.as_ref())
        .is_some_and(|hooks| !hooks.is_empty())
        || vtcode_core::ActivePrimaryAgentState::from_specs_with_default(&discovered.effective, default_primary_agent)
            .active()
            .contributes_workspace_controlled_hooks();
    let file_ops_tool = create_file_ops_tool(&workspace_root, &runtime_config.zed_config);

    Ok(PreparedSessionWorkspace {
        workspace_root,
        session_config,
        runtime_config,
        vt_config,
        system_prompt,
        primary_agents,
        workspace_hooks_gated,
        file_ops_tool,
    })
}

async fn initialize_session_tool_registry(prepared: &PreparedSessionWorkspace<'_>) -> anyhow::Result<CoreToolRegistry> {
    let local_tool_registry = CoreToolRegistry::new(prepared.workspace_root.clone()).await;
    configure_acp_tool_call_limits(&local_tool_registry, prepared.vt_config);
    local_tool_registry
        .apply_tool_runtime_config(&prepared.runtime_config.commands_config, &prepared.runtime_config.tools_config)
        .await?;
    attach_optional_workspace_components(OptionalWorkspaceComponentsSetup {
        registry: &local_tool_registry,
        agent_config: &prepared.session_config,
        custom_providers: &prepared.runtime_config.custom_providers,
        vt_config: prepared.vt_config,
        workspace_gated: prepared.workspace_hooks_gated,
        workspace_root: &prepared.workspace_root,
    })
    .await;
    Ok(local_tool_registry)
}

async fn attach_optional_workspace_components(setup: OptionalWorkspaceComponentsSetup<'_>) {
    let subagent_outcome = Box::pin(attach_acp_subagent_controller(AcpSubagentControllerSetup {
        registry: setup.registry,
        agent_config: setup.agent_config,
        custom_providers: setup.custom_providers,
        vt_config: setup.vt_config,
        workspace_gated: setup.workspace_gated,
    }))
    .await;
    report_optional_setup_outcome("subagent controller", subagent_outcome);
    let mcp_outcome = attach_acp_mcp_client(setup.registry, setup.vt_config, setup.workspace_root).await;
    report_optional_setup_outcome("MCP client", mcp_outcome);
}

fn create_file_ops_tool(workspace_root: &Path, zed_config: &AgentClientProtocolZedConfig) -> Option<FileOpsTool> {
    if zed_config.tools.list_files {
        let search_root = workspace_root.to_path_buf();
        Some(FileOpsTool::new(workspace_root.to_path_buf(), Arc::new(GrepSearchManager::new(search_root))))
    } else {
        None
    }
}

async fn build_acp_tool_surface(
    local_tool_registry: &CoreToolRegistry,
    workspace_root: &Path,
    zed_config: &AgentClientProtocolZedConfig,
    tools_config: &ToolsConfig,
    skip_confirmations: bool,
    list_files_enabled: bool,
) -> AcpWorkspaceToolSurface {
    let local_definitions = local_tool_registry
        .model_tools(
            SessionToolsConfig::full_public(
                SessionSurface::Acp,
                CapabilityLevel::CodeSearch,
                ToolDocumentationMode::default(),
                ToolModelCapabilities::default(),
            )
            .with_tool_profile(tools_config.profile),
        )
        .await;
    let registry = Arc::new(AcpToolRegistry::new(
        workspace_root,
        zed_config.tools.read_file,
        list_files_enabled,
        local_definitions,
    ));
    let permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync> = Arc::new(
        DefaultPermissionPrompter::with_skip_confirmations(Arc::clone(&registry) as Arc<_>, skip_confirmations),
    );

    AcpWorkspaceToolSurface { registry, permission_prompter }
}
