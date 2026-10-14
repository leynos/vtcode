//! Builds the Zed ACP agent state and shared session services used by request handlers.
use crate::acp;
use crate::audit::AcpAuditLogger;
use crate::permissions::{AcpPermissionPrompter, DefaultPermissionPrompter};
use crate::tooling::AcpToolRegistry;
use crate::zed::connection::ConnectionHandle;
use crate::zed::provider_runtime::ProviderRuntimeRegistry;
use anyhow::Context;
use hashbrown::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;
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
mod task_lifecycle;
mod task_progress;
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
    workspace_runtimes: tokio::sync::Mutex<HashMap<PathBuf, Arc<OnceCell<Arc<SessionWorkspaceRuntime>>>>>,
    workspace_trust_synchronizer: Arc<dyn crate::workspace::WorkspaceTrustSynchronizer + Send + Sync>,
}

pub(crate) struct SessionWorkspaceRuntime {
    workspace_root: PathBuf,
    workspace_hooks_gated: bool,
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

#[derive(Debug, PartialEq, Eq)]
enum OptionalSetupOutcome {
    DisabledByConfiguration,
    DisabledByPolicy,
    Attached,
    AttachedWithDegradation(Vec<String>),
    Failed(String),
}

enum AcpSubagentConfiguration {
    DisabledByConfiguration,
    DisabledByPolicy,
    Ready(Box<VTCodeConfig>),
}

impl OptionalSetupOutcome {
    fn attached_with_degradation(failures: Vec<String>) -> Self {
        if failures.is_empty() {
            Self::Attached
        } else {
            Self::AttachedWithDegradation(failures)
        }
    }
}

fn report_optional_setup_outcome(component: &'static str, outcome: OptionalSetupOutcome) {
    match outcome {
        OptionalSetupOutcome::DisabledByConfiguration => {
            debug!(component, "Optional ACP component is disabled by configuration");
        }
        OptionalSetupOutcome::DisabledByPolicy => {
            debug!(component, "Optional ACP component is disabled by runtime policy");
        }
        OptionalSetupOutcome::Attached => {
            debug!(component, "Optional ACP component attached successfully");
        }
        OptionalSetupOutcome::AttachedWithDegradation(failures) => {
            warn!(component, failures = %failures.join("; "), "Optional ACP component attached with degraded setup");
        }
        OptionalSetupOutcome::Failed(reason) => {
            warn!(component, %reason, "Optional ACP component setup failed; continuing without it");
        }
    }
}

async fn attach_acp_subagent_controller(
    registry: &CoreToolRegistry,
    config: &CoreAgentConfig,
    custom_providers: &[CustomProviderConfig],
    vt_cfg: Option<&VTCodeConfig>,
    workspace_gated: bool,
) -> OptionalSetupOutcome {
    let controller_vt_cfg = match configure_acp_subagents(vt_cfg, config, custom_providers) {
        AcpSubagentConfiguration::DisabledByConfiguration => return OptionalSetupOutcome::DisabledByConfiguration,
        AcpSubagentConfiguration::DisabledByPolicy => return OptionalSetupOutcome::DisabledByPolicy,
        AcpSubagentConfiguration::Ready(config) => *config,
    };

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
        workspace_gated,
        exec_sessions: registry.exec_session_manager(),
        pty_manager: registry.pty_manager().clone(),
        managed_background_runtime: false,
    };
    let controller = match SubagentController::new(controller_config).await {
        Ok(controller) => Arc::new(controller),
        Err(error) => return OptionalSetupOutcome::Failed(error.to_string()),
    };
    let failures = restore_configured_background_subagents(&controller, &controller_vt_cfg).await;
    registry.set_subagent_controller(controller);
    OptionalSetupOutcome::attached_with_degradation(failures)
}

fn configure_acp_subagents(
    vt_cfg: Option<&VTCodeConfig>,
    config: &CoreAgentConfig,
    custom_providers: &[CustomProviderConfig],
) -> AcpSubagentConfiguration {
    let Some(mut controller_vt_cfg) = vt_cfg.filter(|config| config.subagents.enabled).cloned() else {
        return AcpSubagentConfiguration::DisabledByConfiguration;
    };
    let max_in_flight = custom_providers
        .iter()
        .find(|provider| provider.name.eq_ignore_ascii_case(&config.provider))
        .and_then(|provider| provider.request_policy.max_in_flight_requests);
    let Some(max_in_flight) = max_in_flight else {
        return AcpSubagentConfiguration::Ready(Box::new(controller_vt_cfg));
    };
    let Some(max_subagents) =
        effective_acp_subagent_concurrency(controller_vt_cfg.subagents.max_concurrent, Some(max_in_flight))
    else {
        return AcpSubagentConfiguration::DisabledByPolicy;
    };
    if controller_vt_cfg.subagents.max_concurrent > max_subagents {
        debug!(
            provider = %config.provider,
            max_in_flight,
            configured_subagents = controller_vt_cfg.subagents.max_concurrent,
            effective_subagents = max_subagents,
            "Capping ACP subagent concurrency to preserve provider request capacity"
        );
        controller_vt_cfg.subagents.max_concurrent = max_subagents;
    }
    AcpSubagentConfiguration::Ready(Box::new(controller_vt_cfg))
}

async fn restore_configured_background_subagents(
    controller: &SubagentController,
    config: &VTCodeConfig,
) -> Vec<String> {
    if !config.subagents.background.auto_restore {
        return Vec::new();
    }
    controller
        .restore_background_subagents()
        .await
        .err()
        .map(|error| vec![format!("failed to restore background subagents: {error}")])
        .unwrap_or_default()
}

/// Attach the parent session's MCP client and eagerly discover its tools.
///
/// ACP model tools are snapshotted during [`ZedAgent::new`], so MCP must be
/// initialized before the local registry is projected into ACP definitions.
/// The client is owned exclusively by this parent registry; subagent
/// controllers construct their own clients from their resolved child config.
#[tracing::instrument(skip_all, fields(mcp_enabled = vt_cfg.is_some_and(|config| config.mcp.enabled)))]
async fn attach_acp_mcp_client(
    registry: &CoreToolRegistry,
    vt_cfg: Option<&VTCodeConfig>,
    workspace_root: &std::path::Path,
) -> OptionalSetupOutcome {
    let Some(config) = vt_cfg.filter(|config| config.mcp.enabled) else {
        return OptionalSetupOutcome::DisabledByConfiguration;
    };
    let setup = match prepare_acp_mcp_setup(config, workspace_root).await {
        Ok(setup) => setup,
        Err(reason) => return OptionalSetupOutcome::Failed(reason),
    };
    let AcpMcpSetupPlan {
        config,
        sandbox_context,
        enabled_provider_count,
        mut failures,
    } = setup;
    let mut client = McpClient::with_sandbox_context(config, sandbox_context);
    match client.initialize().await {
        Ok(()) => {}
        Err(error) => {
            return OptionalSetupOutcome::Failed(format!("MCP client initialization failed: {error}"));
        }
    }
    let initialized_provider_count = client.get_status().provider_count;
    if initialized_provider_count < enabled_provider_count {
        failures.push(format!(
            "initialized {initialized_provider_count} of {enabled_provider_count} configured MCP providers"
        ));
    }
    let client = Arc::new(client);
    registry.set_mcp_client(client).await;
    if let Err(error) = registry.refresh_mcp_tools().await {
        failures.push(format!("failed to register MCP proxy tools: {error}"));
    }
    OptionalSetupOutcome::attached_with_degradation(failures)
}

struct AcpMcpSetupPlan {
    config: vtcode_config::mcp::McpClientConfig,
    sandbox_context: Option<McpSandboxContext>,
    enabled_provider_count: usize,
    failures: Vec<String>,
}

async fn prepare_acp_mcp_setup(
    config: &VTCodeConfig,
    workspace_root: &std::path::Path,
) -> Result<AcpMcpSetupPlan, String> {
    let (plugin_providers, mut failures) = discover_acp_plugin_mcp_providers(workspace_root).await;
    let mcp_config = effective_acp_mcp_config(config, plugin_providers);
    if let Err(error) = validate_mcp_config(&mcp_config) {
        failures.push(format!("MCP configuration validation failed: {error}"));
    }
    let sandbox_context = acp_mcp_sandbox_context(config, workspace_root)
        .map_err(|error| format!("unable to construct MCP sandbox policy: {error}"))?;
    let enabled_provider_count = mcp_config.providers.iter().filter(|provider| provider.enabled).count();
    Ok(AcpMcpSetupPlan {
        config: mcp_config,
        sandbox_context,
        enabled_provider_count,
        failures,
    })
}

async fn discover_acp_plugin_mcp_providers(
    workspace_root: &std::path::Path,
) -> (Vec<vtcode_config::mcp::McpProviderConfig>, Vec<String>) {
    let plugin_workspace = workspace_root.to_path_buf();
    match tokio::task::spawn_blocking(move || discover_plugin_mcp_providers(&plugin_workspace)).await {
        Ok(providers) => (providers, Vec::new()),
        Err(error) => (Vec::new(), vec![format!("failed to discover Agent Plugin MCP providers: {error}")]),
    }
}

fn acp_mcp_sandbox_context(
    config: &VTCodeConfig,
    workspace_root: &std::path::Path,
) -> anyhow::Result<Option<McpSandboxContext>> {
    if config.sandbox.enabled
        && !matches!(config.sandbox.default_policy, vtcode_config::SandboxPolicy::DangerFullAccess)
    {
        // Never fall back to an unsandboxed stdio launch when policy construction fails.
        let policy = sandbox_policy_from_runtime_config(&config.sandbox, workspace_root)?;
        Ok(Some(McpSandboxContext::new(policy, workspace_root)))
    } else {
        Ok(None)
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
        workspace_root: PathBuf,
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
        let workspace_hooks_gated = vt_cfg
            .and_then(|config| config.workspace_lifecycle_hooks.as_ref())
            .is_some_and(|hooks| !hooks.is_empty())
            || vtcode_core::ActivePrimaryAgentState::from_specs_with_default(
                &discovered.effective,
                default_primary_agent,
            )
            .active()
            .contributes_workspace_controlled_hooks();
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
        let subagent_outcome = Box::pin(attach_acp_subagent_controller(
            &local_tool_registry,
            &session_config,
            &runtime_config.custom_providers,
            vt_cfg,
            workspace_hooks_gated,
        ))
        .await;
        report_optional_setup_outcome("subagent controller", subagent_outcome);
        let mcp_outcome = attach_acp_mcp_client(&local_tool_registry, vt_cfg, &workspace_root).await;
        report_optional_setup_outcome("MCP client", mcp_outcome);
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
            workspace_hooks_gated,
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
    async fn workspace_runtime_for(&self, workspace_root: PathBuf) -> anyhow::Result<Arc<SessionWorkspaceRuntime>> {
        let workspace_root =
            vtcode_commons::fs::canonicalize_with_context_async(&workspace_root, "ACP session workspace").await?;
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
                SessionWorkspaceRuntime::build(
                    &self.config,
                    workspace_root,
                    &self.workspace_runtime_config,
                    self.vt_config.as_deref(),
                )
                .await
                .map(Arc::new)
            })
            .await?;
        Ok(Arc::clone(runtime))
    }
}

impl ZedAgent {
    #[expect(
        clippy::too_many_arguments,
        reason = "ACP startup composes independent protocol, provider, tool, credential, and UI configuration roots."
    )]
    pub(crate) async fn new(
        mut config: CoreAgentConfig,
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
        if let Ok(workspace_root) = vtcode_commons::canonicalize(&config.workspace) {
            config.workspace = workspace_root;
        }
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
        let workspace_hooks_gated = vt_cfg
            .and_then(|config| config.workspace_lifecycle_hooks.as_ref())
            .is_some_and(|hooks| !hooks.is_empty());
        let subagent_outcome = Box::pin(attach_acp_subagent_controller(
            &core_tool_registry,
            &config,
            custom_providers,
            vt_cfg,
            workspace_hooks_gated,
        ))
        .await;
        report_optional_setup_outcome("subagent controller", subagent_outcome);
        let mcp_outcome = attach_acp_mcp_client(&core_tool_registry, vt_cfg, workspace_root.as_path()).await;
        report_optional_setup_outcome("MCP client", mcp_outcome);
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

        let launch_runtime = Arc::new(SessionWorkspaceRuntime {
            workspace_root: workspace_root.clone(),
            workspace_hooks_gated,
            system_prompt: system_prompt.clone(),
            primary_agents: primary_agents.clone(),
            acp_tool_registry: Arc::clone(&acp_tool_registry),
            permission_prompter: Arc::clone(&permission_prompter),
            local_tool_registry: core_tool_registry.clone(),
            file_ops_tool: file_ops_tool.clone(),
        });
        let mut workspace_runtimes = HashMap::new();
        if let Ok(canonical_workspace) = vtcode_commons::canonicalize(&workspace_root) {
            let runtime_cell = Arc::new(OnceCell::new());
            if runtime_cell.set(launch_runtime).is_ok() {
                drop(workspace_runtimes.insert(canonical_workspace, runtime_cell));
            }
        }
        let provider_metrics = core_tool_registry.metrics_collector();

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
            provider_runtime: ProviderRuntimeRegistry::new(custom_providers, &provider_timeouts, provider_metrics),
            provider_timeouts,
            vt_config: vt_cfg.cloned().map(Box::new),
            audit_logger,
            workspace_runtime_config,
            workspace_runtimes: tokio::sync::Mutex::new(workspace_runtimes),
            workspace_trust_synchronizer: Arc::new(crate::workspace::DefaultWorkspaceTrustSynchronizer::new()),
        }
    }

    fn session_subagent_controller(&self, session: &SessionHandle) -> anyhow::Result<Option<Arc<SubagentController>>> {
        Ok(session
            .workspace_runtime()
            .context("reading ACP session workspace runtime for subagent controller")?
            .and_then(|runtime| runtime.local_tool_registry.subagent_controller())
            .or_else(|| self.local_tool_registry.subagent_controller()))
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
