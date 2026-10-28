//! Workspace-scoped ACP registries, prompts, permissions, and runtime setup.
use anyhow::Context;
use futures::future::BoxFuture;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
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
    AcpMcpClientAttacher, AcpSubagentControllerAttacher, AcpSubagentControllerSetup, DefaultAcpMcpClientAttacher,
    DefaultAcpSubagentControllerAttacher, OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes, ZedAgent,
    ZedAgentSetup, configure_acp_tool_call_limits, report_optional_workspace_setup_outcomes,
};
use crate::permissions::{AcpPermissionPrompter, DefaultPermissionPrompter};
use crate::tooling::AcpToolRegistry;

mod runtime;
use runtime::WorkspaceRuntimeLookup;
pub(in crate::zed) use runtime::{SessionWorkspaceLease, SessionWorkspaceRuntime};
pub(super) use runtime::{WorkspaceRuntimeCache, WorkspaceRuntimeLifecycle, WorkspaceRuntimeLifecycleState};

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

struct PreparedWorkspace<'a> {
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

struct InitializedWorkspaceToolRegistry {
    registry: CoreToolRegistry,
    optional_setup_outcomes: OptionalWorkspaceSetupOutcomes,
}

struct OptionalWorkspaceComponentsSetup<'a> {
    registry: &'a CoreToolRegistry,
    agent_config: &'a CoreAgentConfig,
    custom_providers: &'a [CustomProviderConfig],
    vt_config: Option<&'a VTCodeConfig>,
    workspace_gated: bool,
    workspace_root: &'a Path,
}

/// Port for discovering the primary agents associated with one ACP workspace.
pub(super) trait WorkspaceAgentDiscovery: Send + Sync {
    fn discover(
        &self,
        input: &SubagentDiscoveryInput,
        default_primary_agent: &str,
    ) -> anyhow::Result<PrimaryAgentCatalog>;
}

/// Port for creating and configuring a workspace's core tool registry.
pub(super) trait WorkspaceToolRegistrySetup: Send + Sync {
    fn initialize<'a>(
        &'a self,
        workspace_root: &'a Path,
        runtime_config: &'a SessionWorkspaceRuntimeConfig,
        vt_config: Option<&'a VTCodeConfig>,
    ) -> BoxFuture<'a, anyhow::Result<CoreToolRegistry>>;
}

/// Port for constructing the optional workspace file-operations tool.
pub(super) trait WorkspaceFileOpsToolConstruction: Send + Sync {
    fn construct(
        &self,
        workspace_root: &Path,
        zed_config: &AgentClientProtocolZedConfig,
    ) -> anyhow::Result<Option<FileOpsTool>>;
}

/// Port for projecting the core registry into the ACP model-facing tool surface.
pub(super) trait AcpWorkspaceToolProjection: Send + Sync {
    fn project<'a>(
        &'a self,
        local_tool_registry: &'a CoreToolRegistry,
        workspace_root: &'a Path,
        zed_config: &'a AgentClientProtocolZedConfig,
        tools_config: &'a ToolsConfig,
        list_files_enabled: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<AcpToolRegistry>>>;
}

/// Port for building the permission policy around the projected ACP tools.
pub(super) trait AcpPermissionPrompterConstruction: Send + Sync {
    fn construct(
        &self,
        registry: Arc<AcpToolRegistry>,
        skip_confirmations: bool,
    ) -> anyhow::Result<Arc<dyn AcpPermissionPrompter + Send + Sync>>;
}

/// The purpose-shaped adapters used at the workspace runtime composition root.
#[derive(Clone)]
pub(super) struct WorkspaceSetupPorts {
    agent_discovery: Arc<dyn WorkspaceAgentDiscovery>,
    registry_setup: Arc<dyn WorkspaceToolRegistrySetup>,
    file_ops_tool_construction: Arc<dyn WorkspaceFileOpsToolConstruction>,
    subagent_attachment: Arc<dyn AcpSubagentControllerAttacher>,
    mcp_attachment: Arc<dyn AcpMcpClientAttacher>,
    tool_projection: Arc<dyn AcpWorkspaceToolProjection>,
    permission_construction: Arc<dyn AcpPermissionPrompterConstruction>,
}

impl Default for WorkspaceSetupPorts {
    fn default() -> Self {
        Self {
            agent_discovery: Arc::new(DefaultWorkspaceAgentDiscovery),
            registry_setup: Arc::new(DefaultWorkspaceToolRegistrySetup),
            file_ops_tool_construction: Arc::new(DefaultWorkspaceFileOpsToolConstruction),
            subagent_attachment: Arc::new(DefaultAcpSubagentControllerAttacher),
            mcp_attachment: Arc::new(DefaultAcpMcpClientAttacher),
            tool_projection: Arc::new(DefaultAcpWorkspaceToolProjection),
            permission_construction: Arc::new(DefaultAcpPermissionPrompterConstruction),
        }
    }
}

struct DefaultWorkspaceAgentDiscovery;

impl WorkspaceAgentDiscovery for DefaultWorkspaceAgentDiscovery {
    fn discover(
        &self,
        input: &SubagentDiscoveryInput,
        default_primary_agent: &str,
    ) -> anyhow::Result<PrimaryAgentCatalog> {
        let discovered = discover_subagents(input)?;
        Ok(PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent))
    }
}

struct DefaultWorkspaceToolRegistrySetup;

impl WorkspaceToolRegistrySetup for DefaultWorkspaceToolRegistrySetup {
    fn initialize<'a>(
        &'a self,
        workspace_root: &'a Path,
        runtime_config: &'a SessionWorkspaceRuntimeConfig,
        vt_config: Option<&'a VTCodeConfig>,
    ) -> BoxFuture<'a, anyhow::Result<CoreToolRegistry>> {
        Box::pin(async move {
            let local_tool_registry = CoreToolRegistry::new(workspace_root.to_path_buf()).await;
            configure_acp_tool_call_limits(&local_tool_registry, vt_config);
            local_tool_registry
                .apply_tool_runtime_config(&runtime_config.commands_config, &runtime_config.tools_config)
                .await
                .context("applying tool runtime configuration to ACP workspace registry")?;
            Ok(local_tool_registry)
        })
    }
}

struct DefaultWorkspaceFileOpsToolConstruction;

impl WorkspaceFileOpsToolConstruction for DefaultWorkspaceFileOpsToolConstruction {
    fn construct(
        &self,
        workspace_root: &Path,
        zed_config: &AgentClientProtocolZedConfig,
    ) -> anyhow::Result<Option<FileOpsTool>> {
        Ok(create_file_ops_tool(workspace_root, zed_config))
    }
}

struct DefaultAcpWorkspaceToolProjection;

impl AcpWorkspaceToolProjection for DefaultAcpWorkspaceToolProjection {
    fn project<'a>(
        &'a self,
        local_tool_registry: &'a CoreToolRegistry,
        workspace_root: &'a Path,
        zed_config: &'a AgentClientProtocolZedConfig,
        tools_config: &'a ToolsConfig,
        list_files_enabled: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<AcpToolRegistry>>> {
        Box::pin(async move {
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
            Ok(Arc::new(AcpToolRegistry::new(
                workspace_root,
                zed_config.tools.read_file,
                list_files_enabled,
                local_definitions,
            )))
        })
    }
}

struct DefaultAcpPermissionPrompterConstruction;

impl AcpPermissionPrompterConstruction for DefaultAcpPermissionPrompterConstruction {
    fn construct(
        &self,
        registry: Arc<AcpToolRegistry>,
        skip_confirmations: bool,
    ) -> anyhow::Result<Arc<dyn AcpPermissionPrompter + Send + Sync>> {
        Ok(Arc::new(DefaultPermissionPrompter::with_skip_confirmations(
            Arc::clone(&registry) as Arc<_>,
            skip_confirmations,
        )))
    }
}

pub(super) async fn build_launch_workspace_runtime(
    setup: &ZedAgentSetup<'_>,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<SessionWorkspaceRuntime> {
    let runtime_config = SessionWorkspaceRuntimeConfig {
        zed_config: setup.zed_config.clone(),
        tools_config: setup.tools_config.clone(),
        commands_config: setup.commands_config.clone(),
        custom_providers: setup.custom_providers.to_vec(),
        skip_confirmations: setup.skip_confirmations,
    };
    let prepared = prepare_launch_workspace(setup, &runtime_config, ports)?;
    build_prepared_workspace_runtime(prepared, false, ports).await
}

impl SessionWorkspaceRuntime {
    async fn build(setup: SessionWorkspaceRuntimeSetup<'_>, ports: &WorkspaceSetupPorts) -> anyhow::Result<Self> {
        let prepared = prepare_session_workspace(setup, ports).await?;
        build_prepared_workspace_runtime(prepared, true, ports).await
    }

    fn report_optional_setup_outcomes(&self) {
        report_optional_workspace_setup_outcomes(&self.optional_setup_outcomes);
    }
}

fn build_session_workspace_runtime<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
    ports: &'a WorkspaceSetupPorts,
) -> BoxFuture<'a, anyhow::Result<Arc<SessionWorkspaceRuntime>>> {
    Box::pin(async move { SessionWorkspaceRuntime::build(setup, ports).await.map(Arc::new) })
}

impl ZedAgent {
    pub(super) async fn workspace_runtime_for(
        &self,
        workspace: &AcpSessionWorkspace,
    ) -> anyhow::Result<SessionWorkspaceLease> {
        let workspace_root = workspace.path().to_path_buf();
        if self.launch_workspace_runtime.workspace_root == workspace_root {
            return Ok(SessionWorkspaceLease::unmanaged(Arc::clone(&self.launch_workspace_runtime)));
        }
        loop {
            let lookup = {
                let mut runtimes = self.workspace_runtimes.lock().await;
                runtimes.lookup_or_reserve_initialization(&workspace_root)
            };
            let (runtime, runtime_cell) = match lookup {
                WorkspaceRuntimeLookup::Ready(runtime) => (runtime, None),
                WorkspaceRuntimeLookup::Initialize(runtime_cell) => {
                    let runtime_result = runtime_cell
                        .get_or_try_init(|| {
                            build_session_workspace_runtime(
                                SessionWorkspaceRuntimeSetup {
                                    base_config: &self.config,
                                    workspace,
                                    runtime_config: &self.workspace_runtime_config,
                                    vt_config: self.vt_config.as_deref(),
                                },
                                &self.workspace_setup_ports,
                            )
                        })
                        .await;
                    match runtime_result {
                        Ok(runtime) => {
                            let runtime = Arc::clone(runtime);
                            self.workspace_runtimes.lock().await.store_initialized_runtime(
                                workspace_root.clone(),
                                &runtime,
                                &runtime_cell,
                            );
                            (runtime, Some(runtime_cell))
                        }
                        Err(error) => {
                            self.workspace_runtimes
                                .lock()
                                .await
                                .remove_in_flight_if_same(&workspace_root, &runtime_cell);
                            return Err(error);
                        }
                    }
                }
            };

            if let Some(lease) = SessionWorkspaceLease::acquire(Arc::clone(&runtime)).await? {
                return Ok(lease);
            }
            self.workspace_runtimes
                .lock()
                .await
                .remove_runtime_if_same(&workspace_root, &runtime);
            drop(runtime_cell);
        }
    }
}

fn prepare_launch_workspace<'a>(
    setup: &ZedAgentSetup<'a>,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<PreparedWorkspace<'a>> {
    let workspace_root = setup.config.workspace.clone();
    let workspace_hooks_gated = setup.primary_agents.workspace_hooks_gated(setup.vt_config);
    let file_ops_tool = construct_workspace_file_ops_tool(
        ports.file_ops_tool_construction.as_ref(),
        &workspace_root,
        &runtime_config.zed_config,
    )?;

    Ok(PreparedWorkspace {
        workspace_root,
        session_config: setup.config.clone(),
        runtime_config,
        vt_config: setup.vt_config,
        system_prompt: setup.system_prompt.clone(),
        primary_agents: setup.primary_agents.clone(),
        workspace_hooks_gated,
        file_ops_tool,
    })
}

async fn prepare_session_workspace<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<PreparedWorkspace<'a>> {
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
    let default_primary_agent = vt_config.map_or("duck", |config| config.default_primary_agent.as_str());
    let discovery_input = SubagentDiscoveryInput::new(workspace_root.clone());
    let primary_agents =
        discover_session_primary_agents_with(ports.agent_discovery.as_ref(), &discovery_input, default_primary_agent)?;
    let workspace_hooks_gated = primary_agents.workspace_hooks_gated(vt_config);
    let file_ops_tool = construct_workspace_file_ops_tool(
        ports.file_ops_tool_construction.as_ref(),
        &workspace_root,
        &runtime_config.zed_config,
    )?;

    Ok(PreparedWorkspace {
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

#[cfg(test)]
fn discover_session_primary_agents(
    input: &SubagentDiscoveryInput,
    default_primary_agent: &str,
) -> anyhow::Result<PrimaryAgentCatalog> {
    discover_session_primary_agents_with(&DefaultWorkspaceAgentDiscovery, input, default_primary_agent)
}

fn discover_session_primary_agents_with(
    agent_discovery: &dyn WorkspaceAgentDiscovery,
    input: &SubagentDiscoveryInput,
    default_primary_agent: &str,
) -> anyhow::Result<PrimaryAgentCatalog> {
    agent_discovery
        .discover(input, default_primary_agent)
        .context("discovering ACP workspace agents")
}

async fn initialize_workspace_tool_registry(
    prepared: &PreparedWorkspace<'_>,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<InitializedWorkspaceToolRegistry> {
    let local_tool_registry = ports
        .registry_setup
        .initialize(&prepared.workspace_root, prepared.runtime_config, prepared.vt_config)
        .await
        .context("setting up ACP workspace tool registry")?;
    let optional_setup_outcomes = attach_optional_workspace_components(
        OptionalWorkspaceComponentsSetup {
            registry: &local_tool_registry,
            agent_config: &prepared.session_config,
            custom_providers: &prepared.runtime_config.custom_providers,
            vt_config: prepared.vt_config,
            workspace_gated: prepared.workspace_hooks_gated,
            workspace_root: &prepared.workspace_root,
        },
        ports,
    )
    .await;
    Ok(InitializedWorkspaceToolRegistry {
        registry: local_tool_registry,
        optional_setup_outcomes,
    })
}

async fn build_prepared_workspace_runtime(
    prepared: PreparedWorkspace<'_>,
    shutdown_managed: bool,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<SessionWorkspaceRuntime> {
    let initialized_registry = initialize_workspace_tool_registry(&prepared, ports).await?;
    let tool_surface = build_acp_tool_surface(
        &initialized_registry.registry,
        &prepared.workspace_root,
        &prepared.runtime_config.zed_config,
        &prepared.runtime_config.tools_config,
        prepared.runtime_config.skip_confirmations,
        prepared.file_ops_tool.is_some(),
        ports,
    )
    .await?;

    let runtime = SessionWorkspaceRuntime {
        workspace_root: prepared.workspace_root,
        workspace_hooks_gated: prepared.workspace_hooks_gated,
        system_prompt: prepared.system_prompt,
        primary_agents: prepared.primary_agents,
        acp_tool_registry: tool_surface.registry,
        permission_prompter: tool_surface.permission_prompter,
        local_tool_registry: initialized_registry.registry,
        file_ops_tool: prepared.file_ops_tool,
        optional_setup_outcomes: initialized_registry.optional_setup_outcomes,
        shutdown_managed,
        lifecycle: Mutex::new(WorkspaceRuntimeLifecycle {
            state: WorkspaceRuntimeLifecycleState::Open,
            active_sessions: 0,
        }),
        lifecycle_changed: Notify::new(),
    };
    runtime.report_optional_setup_outcomes();
    Ok(runtime)
}

async fn attach_optional_workspace_components(
    setup: OptionalWorkspaceComponentsSetup<'_>,
    ports: &WorkspaceSetupPorts,
) -> OptionalWorkspaceSetupOutcomes {
    let subagent_outcome = ports
        .subagent_attachment
        .attach(AcpSubagentControllerSetup {
            registry: setup.registry,
            agent_config: setup.agent_config,
            custom_providers: setup.custom_providers,
            vt_config: setup.vt_config,
            workspace_gated: setup.workspace_gated,
        })
        .await;
    let mcp_outcome = ports
        .mcp_attachment
        .attach(setup.registry, setup.vt_config, setup.workspace_root)
        .await;
    preserve_optional_workspace_setup_outcomes(subagent_outcome, mcp_outcome)
}

fn preserve_optional_workspace_setup_outcomes(
    subagent_controller: OptionalSetupOutcome,
    mcp_client: OptionalSetupOutcome,
) -> OptionalWorkspaceSetupOutcomes {
    OptionalWorkspaceSetupOutcomes { subagent_controller, mcp_client }
}

fn create_file_ops_tool(workspace_root: &Path, zed_config: &AgentClientProtocolZedConfig) -> Option<FileOpsTool> {
    if zed_config.tools.list_files {
        let search_root = workspace_root.to_path_buf();
        Some(FileOpsTool::new(workspace_root.to_path_buf(), Arc::new(GrepSearchManager::new(search_root))))
    } else {
        None
    }
}

fn construct_workspace_file_ops_tool(
    construction: &dyn WorkspaceFileOpsToolConstruction,
    workspace_root: &Path,
    zed_config: &AgentClientProtocolZedConfig,
) -> anyhow::Result<Option<FileOpsTool>> {
    construction
        .construct(workspace_root, zed_config)
        .context("constructing ACP workspace file operations tool")
}

async fn build_acp_tool_surface(
    local_tool_registry: &CoreToolRegistry,
    workspace_root: &Path,
    zed_config: &AgentClientProtocolZedConfig,
    tools_config: &ToolsConfig,
    skip_confirmations: bool,
    list_files_enabled: bool,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<AcpWorkspaceToolSurface> {
    let registry = ports
        .tool_projection
        .project(local_tool_registry, workspace_root, zed_config, tools_config, list_files_enabled)
        .await
        .context("projecting ACP workspace tool surface")?;
    let permission_prompter = ports
        .permission_construction
        .construct(Arc::clone(&registry), skip_confirmations)
        .context("constructing ACP workspace permission prompter")?;

    Ok(AcpWorkspaceToolSurface { registry, permission_prompter })
}

#[cfg(test)]
mod setup_tests;
#[cfg(test)]
mod tests;
