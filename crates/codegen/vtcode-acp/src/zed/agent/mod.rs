use crate::acp;
use crate::audit::AcpAuditLogger;
use crate::permissions::{AcpPermissionPrompter, DefaultPermissionPrompter};
use crate::tooling::AcpToolRegistry;
use crate::zed::connection::ConnectionHandle;
use crate::zed::provider_runtime::ProviderRuntimeRegistry;
use hashbrown::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::CustomProviderConfig;
use vtcode_config::{SubagentDiscoveryInput, TimeoutsConfig, discover_subagents};
use vtcode_core::config::ToolDocumentationMode;
use vtcode_core::config::types::{AgentConfig as CoreAgentConfig, CapabilityLevel};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::threads::ThreadManager;
use vtcode_core::mcp::plugin_providers::discover_plugin_mcp_providers;
use vtcode_core::mcp::{McpClient, McpSandboxContext, validate_mcp_config};
use vtcode_core::prompts::system::generate_system_instruction_with_config;
use vtcode_core::subagents::{SubagentController, SubagentControllerConfig};
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::grep_file::GrepSearchManager;
use vtcode_core::tools::handlers::{SessionSurface, SessionToolsConfig, ToolModelCapabilities};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;
use vtcode_core::tools::registry::sandbox_policy_from_runtime_config;

use super::helpers::PrimaryAgentCatalog;
use super::types::SessionHandle;

mod compaction;
pub(crate) mod handlers;
mod prompt;
mod session_state;
mod tool_config;
#[cfg(test)]
mod tool_config_tests;
mod tool_execution;
mod tool_execution_local;
mod tool_execution_output;
mod updates;

/// SACP-style agent bridge. `Send + Sync` so it can be moved into SACP
/// `cx.spawn` tasks and held inside the global connection registry.
pub(crate) struct ZedAgent {
    config: CoreAgentConfig,
    credential_storage_mode: AuthCredentialsStoreMode,
    system_prompt: String,
    sessions: Arc<Mutex<HashMap<acp::SessionId, SessionHandle>>>,
    next_session_id: AtomicU64,
    acp_tool_registry: Arc<AcpToolRegistry>,
    permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
    local_tool_registry: CoreToolRegistry,
    file_ops_tool: Option<FileOpsTool>,
    thread_manager: ThreadManager,
    client_capabilities: Arc<Mutex<Option<acp::ClientCapabilities>>>,
    client: Arc<Mutex<Option<Arc<ConnectionHandle>>>>,
    title: Option<String>,
    primary_agents: PrimaryAgentCatalog,
    tool_loop_limit: usize,
    tool_call_delay: Option<Duration>,
    provider_runtime: ProviderRuntimeRegistry,
    provider_timeouts: TimeoutsConfig,
    vt_config: Option<Box<VTCodeConfig>>,
    audit_logger: Option<Arc<AcpAuditLogger>>,
    workspace_runtime_config: WorkspaceRuntimeConfig,
}

pub(crate) struct SessionWorkspaceRuntime {
    workspace_root: std::path::PathBuf,
    system_prompt: String,
    primary_agents: PrimaryAgentCatalog,
    acp_tool_registry: Arc<AcpToolRegistry>,
    permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
    local_tool_registry: CoreToolRegistry,
    file_ops_tool: Option<FileOpsTool>,
}

#[derive(Clone)]
struct WorkspaceRuntimeConfig {
    zed_config: AgentClientProtocolZedConfig,
    tools_config: ToolsConfig,
    commands_config: CommandsConfig,
    custom_providers: Vec<CustomProviderConfig>,
    skip_confirmations: bool,
}

fn effective_acp_subagent_concurrency(configured: usize, max_in_flight: Option<usize>) -> Option<usize> {
    match max_in_flight {
        Some(0 | 1) => None,
        Some(limit) => Some(configured.min(limit - 1)),
        None => Some(configured),
    }
}

async fn attach_acp_subagent_controller(
    registry: &CoreToolRegistry,
    config: &CoreAgentConfig,
    custom_providers: &[CustomProviderConfig],
    vt_cfg: Option<&VTCodeConfig>,
) {
    let Some(mut controller_vt_cfg) = vt_cfg.filter(|config| config.subagents.enabled).cloned() else {
        return;
    };

    if let Some(max_in_flight) = custom_providers
        .iter()
        .find(|provider| provider.name.eq_ignore_ascii_case(&config.provider))
        .and_then(|provider| provider.request_policy.max_in_flight_requests)
    {
        match effective_acp_subagent_concurrency(controller_vt_cfg.subagents.max_concurrent, Some(max_in_flight)) {
            None => {
                warn!(
                    provider = %config.provider,
                    max_in_flight,
                    "ACP subagents disabled because the provider request limit reserves no child capacity"
                );
                return;
            }
            Some(max_subagents) if controller_vt_cfg.subagents.max_concurrent > max_subagents => {
                debug!(
                    provider = %config.provider,
                    max_in_flight,
                    configured_subagents = controller_vt_cfg.subagents.max_concurrent,
                    effective_subagents = max_subagents,
                    "Capping ACP subagent concurrency to preserve provider request capacity"
                );
                controller_vt_cfg.subagents.max_concurrent = max_subagents;
            }
            Some(_) => {}
        }
    }

    let controller_config = SubagentControllerConfig {
        workspace_root: config.workspace.clone(),
        parent_session_id: "vtcode-acp".to_string(),
        parent_model: config.model.clone(),
        parent_provider: config.provider.clone(),
        parent_reasoning_effort: config.reasoning_effort,
        api_key: config.api_key.clone(),
        vt_cfg: controller_vt_cfg.clone(),
        openai_chatgpt_auth: config.openai_chatgpt_auth.clone(),
        depth: 0,
        exec_sessions: registry.exec_session_manager(),
        pty_manager: registry.pty_manager().clone(),
        managed_background_runtime: false,
    };
    match SubagentController::new(controller_config).await {
        Ok(controller) => {
            let controller = Arc::new(controller);
            if controller_vt_cfg.subagents.background.auto_restore
                && let Err(error) = controller.restore_background_subagents().await
            {
                warn!(%error, "Failed to restore ACP background subagents");
            }
            registry.set_subagent_controller(controller);
        }
        Err(error) => warn!(%error, "Failed to initialize ACP subagent controller"),
    }
}

/// Attach the parent session's MCP client and eagerly discover its tools.
///
/// ACP model tools are snapshotted during [`ZedAgent::new`], so MCP must be
/// initialized before the local registry is projected into ACP definitions.
/// The client is owned exclusively by this parent registry; subagent
/// controllers construct their own clients from their resolved child config.
async fn attach_acp_mcp_client(
    registry: &CoreToolRegistry,
    vt_cfg: Option<&VTCodeConfig>,
    workspace_root: &std::path::Path,
) {
    let Some(config) = vt_cfg.filter(|config| config.mcp.enabled) else {
        return;
    };

    let plugin_workspace = workspace_root.to_path_buf();
    let plugin_providers =
        match tokio::task::spawn_blocking(move || discover_plugin_mcp_providers(&plugin_workspace)).await {
            Ok(providers) => providers,
            Err(error) => {
                warn!(%error, "Failed to discover Agent Plugin MCP providers during ACP startup");
                Vec::new()
            }
        };
    let mcp_config = effective_acp_mcp_config(config, plugin_providers);

    if let Err(error) = validate_mcp_config(&mcp_config) {
        warn!(%error, "MCP configuration validation error during ACP startup");
    }

    let sandbox_context = if config.sandbox.enabled
        && !matches!(config.sandbox.default_policy, vtcode_config::SandboxPolicy::DangerFullAccess)
    {
        match sandbox_policy_from_runtime_config(&config.sandbox, workspace_root) {
            Ok(policy) => Some(McpSandboxContext::new(policy, workspace_root)),
            Err(error) => {
                warn!(%error, "Unable to construct the ACP MCP sandbox policy");
                // Never fall back to an unsandboxed stdio launch when the
                // configured sandbox cannot be constructed.
                return;
            }
        }
    } else {
        None
    };

    let mut client = McpClient::with_sandbox_context(mcp_config, sandbox_context);
    match client.initialize().await {
        Ok(()) => debug!("ACP MCP client initialized successfully"),
        Err(error) => {
            warn!(%error, "ACP MCP client initialization failed; continuing without MCP tools");
            return;
        }
    }

    let client = Arc::new(client);
    registry.set_mcp_client(client).await;
    if let Err(error) = registry.refresh_mcp_tools().await {
        warn!(%error, "Failed to register ACP MCP proxy tools after initialization");
    }
}

fn effective_acp_mcp_config(
    config: &VTCodeConfig,
    plugin_providers: impl IntoIterator<Item = vtcode_config::mcp::McpProviderConfig>,
) -> vtcode_config::mcp::McpClientConfig {
    let mut mcp_config = config.mcp.clone();
    mcp_config.providers.extend(plugin_providers);
    mcp_config
}

fn configure_acp_tool_call_limits(registry: &CoreToolRegistry, vt_cfg: Option<&VTCodeConfig>) {
    let Some(harness) = vt_cfg.map(|config| &config.agent.harness) else {
        return;
    };
    let safety_gateway = registry.safety_gateway();
    let max_per_session = harness
        .max_tool_calls_per_session
        .unwrap_or_else(|| safety_gateway.max_per_session());
    safety_gateway.set_limits(harness.max_tool_calls_per_turn, max_per_session);
}

impl SessionWorkspaceRuntime {
    async fn build(
        base_config: &CoreAgentConfig,
        workspace_root: std::path::PathBuf,
        runtime_config: &WorkspaceRuntimeConfig,
        vt_cfg: Option<&VTCodeConfig>,
    ) -> anyhow::Result<Self> {
        let mut session_config = base_config.clone();
        session_config.workspace = workspace_root.clone();
        let content = generate_system_instruction_with_config(&Default::default(), &workspace_root, vt_cfg).await;
        let system_prompt = content
            .parts
            .first()
            .and_then(|part| part.as_text())
            .map_or_else(String::new, ToString::to_string);
        let discovered = discover_subagents(&SubagentDiscoveryInput::new(workspace_root.clone()))?;
        let default_primary_agent = vt_cfg.map_or("duck", |config| config.default_primary_agent.as_str());
        let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent);
        let file_ops_tool = if runtime_config.zed_config.tools.list_files {
            let search_root = workspace_root.clone();
            Some(FileOpsTool::new(workspace_root.clone(), Arc::new(GrepSearchManager::new(search_root))))
        } else {
            None
        };
        let list_files_enabled = file_ops_tool.is_some();
        let local_tool_registry = CoreToolRegistry::new(workspace_root.clone()).await;
        configure_acp_tool_call_limits(&local_tool_registry, vt_cfg);
        local_tool_registry
            .apply_tool_runtime_config(&runtime_config.commands_config, &runtime_config.tools_config)
            .await?;
        Box::pin(attach_acp_subagent_controller(
            &local_tool_registry,
            &session_config,
            &runtime_config.custom_providers,
            vt_cfg,
        ))
        .await;
        attach_acp_mcp_client(&local_tool_registry, vt_cfg, &workspace_root).await;
        let local_definitions = local_tool_registry
            .model_tools(
                SessionToolsConfig::full_public(
                    SessionSurface::Acp,
                    CapabilityLevel::CodeSearch,
                    ToolDocumentationMode::default(),
                    ToolModelCapabilities::default(),
                )
                .with_tool_profile(runtime_config.tools_config.profile),
            )
            .await;
        let acp_tool_registry = Arc::new(AcpToolRegistry::new(
            &workspace_root,
            runtime_config.zed_config.tools.read_file,
            list_files_enabled,
            local_definitions,
        ));
        let permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync> =
            Arc::new(DefaultPermissionPrompter::with_skip_confirmations(
                Arc::clone(&acp_tool_registry) as Arc<_>,
                runtime_config.skip_confirmations,
            ));

        Ok(Self {
            workspace_root,
            system_prompt,
            primary_agents,
            acp_tool_registry,
            permission_prompter,
            local_tool_registry,
            file_ops_tool,
        })
    }
}

impl ZedAgent {
    #[allow(
        clippy::too_many_arguments,
        reason = "ACP startup composes independent protocol, provider, tool, credential, and UI configuration roots."
    )]
    pub(crate) async fn new(
        config: CoreAgentConfig,
        credential_storage_mode: AuthCredentialsStoreMode,
        zed_config: AgentClientProtocolZedConfig,
        tools_config: ToolsConfig,
        commands_config: CommandsConfig,
        custom_providers: &[CustomProviderConfig],
        provider_timeouts: TimeoutsConfig,
        system_prompt: String,
        title: Option<String>,
        primary_agents: PrimaryAgentCatalog,
        skip_confirmations: bool,
        vt_cfg: Option<&VTCodeConfig>,
        audit_logger: Option<Arc<AcpAuditLogger>>,
    ) -> Self {
        let read_file_enabled = zed_config.tools.read_file;
        let workspace_root = config.workspace.clone();
        let tool_loop_limit = tools_config.max_tool_loops;
        let tool_call_delay = tools_config.tool_call_delay();
        let file_ops_tool = if zed_config.tools.list_files {
            let search_root = workspace_root.clone();
            Some(FileOpsTool::new(workspace_root.clone(), Arc::new(GrepSearchManager::new(search_root))))
        } else {
            None
        };
        let list_files_enabled = file_ops_tool.is_some();
        let core_tool_registry = CoreToolRegistry::new(config.workspace.clone()).await;
        configure_acp_tool_call_limits(&core_tool_registry, vt_cfg);
        if let Err(error) = core_tool_registry
            .apply_tool_runtime_config(&commands_config, &tools_config)
            .await
        {
            warn!(%error, "Failed to apply tools configuration to ACP tool registry");
        }
        Box::pin(attach_acp_subagent_controller(&core_tool_registry, &config, custom_providers, vt_cfg)).await;
        attach_acp_mcp_client(&core_tool_registry, vt_cfg, workspace_root.as_path()).await;
        let local_definitions = core_tool_registry
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
        let acp_tool_registry = Arc::new(AcpToolRegistry::new(
            workspace_root.as_path(),
            read_file_enabled,
            list_files_enabled,
            local_definitions,
        ));
        let permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync> =
            Arc::new(DefaultPermissionPrompter::with_skip_confirmations(
                Arc::clone(&acp_tool_registry) as Arc<_>,
                skip_confirmations,
            ));
        let workspace_runtime_config = WorkspaceRuntimeConfig {
            zed_config,
            tools_config,
            commands_config,
            custom_providers: custom_providers.to_vec(),
            skip_confirmations,
        };

        Self {
            config,
            credential_storage_mode,
            system_prompt,
            sessions: Arc::new(Mutex::new(HashMap::with_capacity(10))),
            next_session_id: AtomicU64::new(0),
            acp_tool_registry,
            permission_prompter,
            local_tool_registry: core_tool_registry,
            file_ops_tool,
            thread_manager: ThreadManager::new(),
            client_capabilities: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            title,
            primary_agents,
            tool_loop_limit,
            tool_call_delay,
            provider_runtime: ProviderRuntimeRegistry::new(custom_providers, &provider_timeouts),
            provider_timeouts,
            vt_config: vt_cfg.cloned().map(Box::new),
            audit_logger,
            workspace_runtime_config,
        }
    }

    fn session_subagent_controller(&self, session: &SessionHandle) -> Option<Arc<SubagentController>> {
        session
            .workspace_runtime()
            .and_then(|runtime| runtime.local_tool_registry.subagent_controller())
            .or_else(|| self.local_tool_registry.subagent_controller())
    }

    /// Attach the live SACP `cx` handle. Called once after the SACP
    /// connection has been opened.
    pub(crate) fn attach_client(&self, client: Arc<ConnectionHandle>) {
        if let Ok(mut guard) = self.client.lock() {
            *guard = Some(client);
        }
    }

    /// Borrow the SACP `cx` handle, if one is attached.
    fn client(&self) -> Option<Arc<ConnectionHandle>> {
        self.client.lock().unwrap_or_else(|e| e.into_inner()).as_ref().cloned()
    }

    /// Optional human-readable title used during `initialize`.
    fn title(&self) -> Option<String> {
        self.title.clone()
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::{configure_acp_tool_call_limits, effective_acp_mcp_config, effective_acp_subagent_concurrency};
    use std::path::PathBuf;
    use vtcode_config::mcp::{McpProviderConfig, McpStdioServerConfig, McpTransportConfig};
    use vtcode_core::config::VTCodeConfig;
    use vtcode_core::tools::ToolRegistry;

    #[test]
    fn provider_limit_reserves_one_request_for_the_parent() {
        assert_eq!(effective_acp_subagent_concurrency(3, Some(3)), Some(2));
        assert_eq!(effective_acp_subagent_concurrency(3, Some(6)), Some(3));
        assert_eq!(effective_acp_subagent_concurrency(3, None), Some(3));
    }

    #[test]
    fn single_request_provider_capacity_disables_acp_subagents() {
        assert_eq!(effective_acp_subagent_concurrency(3, Some(1)), None);
    }

    #[tokio::test]
    async fn acp_applies_unlimited_harness_tool_call_budgets() {
        let registry = ToolRegistry::new(PathBuf::from("/tmp/vtcode-acp-tool-limit-test")).await;
        let mut config = VTCodeConfig::default();
        config.agent.harness.max_tool_calls_per_turn = 0;
        config.agent.harness.max_tool_calls_per_session = Some(0);

        configure_acp_tool_call_limits(&registry, Some(&config));

        assert_eq!(registry.safety_gateway().max_per_session(), 0);
    }

    #[test]
    fn acp_mcp_config_preserves_configured_and_plugin_providers() {
        let mut config = VTCodeConfig::default();
        config.mcp.providers.push(McpProviderConfig {
            name: "configured".to_string(),
            transport: McpTransportConfig::Stdio(McpStdioServerConfig::default()),
            ..McpProviderConfig::default()
        });
        let plugin = McpProviderConfig {
            name: "plugin".to_string(),
            transport: McpTransportConfig::Stdio(McpStdioServerConfig::default()),
            ..McpProviderConfig::default()
        };

        let effective = effective_acp_mcp_config(&config, [plugin]);
        let names = effective
            .providers
            .iter()
            .map(|provider| provider.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["configured", "plugin"]);
    }
}
