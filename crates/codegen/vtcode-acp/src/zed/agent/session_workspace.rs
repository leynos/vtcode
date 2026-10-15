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
    AcpSubagentControllerSetup, OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes, ZedAgent, ZedAgentSetup,
    attach_acp_mcp_client, attach_acp_subagent_controller, configure_acp_tool_call_limits,
    report_optional_workspace_setup_outcomes,
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

pub(super) async fn build_launch_workspace_runtime(
    setup: &ZedAgentSetup<'_>,
) -> anyhow::Result<SessionWorkspaceRuntime> {
    let runtime_config = SessionWorkspaceRuntimeConfig {
        zed_config: setup.zed_config.clone(),
        tools_config: setup.tools_config.clone(),
        commands_config: setup.commands_config.clone(),
        custom_providers: setup.custom_providers.to_vec(),
        skip_confirmations: setup.skip_confirmations,
    };
    let prepared = prepare_launch_workspace(setup, &runtime_config);
    build_prepared_workspace_runtime(prepared, false).await
}

impl SessionWorkspaceRuntime {
    async fn build(setup: SessionWorkspaceRuntimeSetup<'_>) -> anyhow::Result<Self> {
        let prepared = prepare_session_workspace(setup).await?;
        build_prepared_workspace_runtime(prepared, true).await
    }

    fn report_optional_setup_outcomes(&self) {
        report_optional_workspace_setup_outcomes(&self.optional_setup_outcomes);
    }
}

fn build_session_workspace_runtime<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
) -> BoxFuture<'a, anyhow::Result<Arc<SessionWorkspaceRuntime>>> {
    Box::pin(async move { SessionWorkspaceRuntime::build(setup).await.map(Arc::new) })
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
                runtimes.lookup(&workspace_root)
            };
            let (runtime, runtime_cell) = match lookup {
                WorkspaceRuntimeLookup::Ready(runtime) => (runtime, None),
                WorkspaceRuntimeLookup::Initialize(runtime_cell) => {
                    let runtime_result = runtime_cell
                        .get_or_try_init(|| {
                            build_session_workspace_runtime(SessionWorkspaceRuntimeSetup {
                                base_config: &self.config,
                                workspace,
                                runtime_config: &self.workspace_runtime_config,
                                vt_config: self.vt_config.as_deref(),
                            })
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
) -> PreparedWorkspace<'a> {
    let workspace_root = setup.config.workspace.clone();
    let workspace_hooks_gated = setup.primary_agents.workspace_hooks_gated(setup.vt_config);
    let file_ops_tool = create_file_ops_tool(&workspace_root, &runtime_config.zed_config);

    PreparedWorkspace {
        workspace_root,
        session_config: setup.config.clone(),
        runtime_config,
        vt_config: setup.vt_config,
        system_prompt: setup.system_prompt.clone(),
        primary_agents: setup.primary_agents.clone(),
        workspace_hooks_gated,
        file_ops_tool,
    }
}

async fn prepare_session_workspace<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
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
    let primary_agents = discover_session_primary_agents(&discovery_input, default_primary_agent)?;
    let workspace_hooks_gated = primary_agents.workspace_hooks_gated(vt_config);
    let file_ops_tool = create_file_ops_tool(&workspace_root, &runtime_config.zed_config);

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

fn discover_session_primary_agents(
    input: &SubagentDiscoveryInput,
    default_primary_agent: &str,
) -> anyhow::Result<PrimaryAgentCatalog> {
    let discovered = discover_subagents(input).context("discovering ACP workspace agents")?;
    Ok(PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent))
}

async fn initialize_workspace_tool_registry(
    prepared: &PreparedWorkspace<'_>,
) -> anyhow::Result<InitializedWorkspaceToolRegistry> {
    let local_tool_registry = CoreToolRegistry::new(prepared.workspace_root.clone()).await;
    configure_acp_tool_call_limits(&local_tool_registry, prepared.vt_config);
    local_tool_registry
        .apply_tool_runtime_config(&prepared.runtime_config.commands_config, &prepared.runtime_config.tools_config)
        .await
        .context("applying tool runtime configuration to ACP workspace registry")?;
    let optional_setup_outcomes = attach_optional_workspace_components(OptionalWorkspaceComponentsSetup {
        registry: &local_tool_registry,
        agent_config: &prepared.session_config,
        custom_providers: &prepared.runtime_config.custom_providers,
        vt_config: prepared.vt_config,
        workspace_gated: prepared.workspace_hooks_gated,
        workspace_root: &prepared.workspace_root,
    })
    .await;
    Ok(InitializedWorkspaceToolRegistry {
        registry: local_tool_registry,
        optional_setup_outcomes,
    })
}

async fn build_prepared_workspace_runtime(
    prepared: PreparedWorkspace<'_>,
    shutdown_managed: bool,
) -> anyhow::Result<SessionWorkspaceRuntime> {
    let initialized_registry = initialize_workspace_tool_registry(&prepared).await?;
    let tool_surface = build_acp_tool_surface(
        &initialized_registry.registry,
        &prepared.workspace_root,
        &prepared.runtime_config.zed_config,
        &prepared.runtime_config.tools_config,
        prepared.runtime_config.skip_confirmations,
        prepared.file_ops_tool.is_some(),
    )
    .await;

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
) -> OptionalWorkspaceSetupOutcomes {
    let subagent_outcome = attach_acp_subagent_controller(AcpSubagentControllerSetup {
        registry: setup.registry,
        agent_config: setup.agent_config,
        custom_providers: setup.custom_providers,
        vt_config: setup.vt_config,
        workspace_gated: setup.workspace_gated,
    })
    .await;
    let mcp_outcome = attach_acp_mcp_client(setup.registry, setup.vt_config, setup.workspace_root).await;
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

#[cfg(test)]
mod tests;
