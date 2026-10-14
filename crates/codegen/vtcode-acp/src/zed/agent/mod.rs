//! Builds the Zed ACP agent state and shared session services used by request handlers.
use crate::acp;
use crate::audit::AcpAuditLogger;
use crate::permissions::AcpPermissionPrompter;
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
use vtcode_config::TimeoutsConfig;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::CustomProviderConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::threads::ThreadManager;
use vtcode_core::subagents::SubagentController;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

use super::helpers::PrimaryAgentCatalog;
use super::types::SessionHandle;

mod optional_setup;
mod session_workspace;
use optional_setup::{
    AcpSubagentControllerSetup, OptionalSetupOutcome, attach_acp_mcp_client, attach_acp_subagent_controller,
    configure_acp_tool_call_limits, effective_acp_mcp_config, effective_acp_subagent_concurrency,
    report_optional_setup_outcome,
};
pub(super) use session_workspace::SessionWorkspaceRuntime;
use session_workspace::{SessionWorkspaceRuntimeConfig, build_launch_workspace_runtime};

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
    workspace_runtime_config: SessionWorkspaceRuntimeConfig,
    workspace_runtimes: tokio::sync::Mutex<HashMap<PathBuf, Arc<OnceCell<Arc<SessionWorkspaceRuntime>>>>>,
    workspace_trust_synchronizer: Arc<dyn crate::workspace::WorkspaceTrustSynchronizer + Send + Sync>,
}

pub(crate) struct ZedAgentSetup<'a> {
    pub(crate) config: CoreAgentConfig,
    pub(crate) credential_storage_mode: AuthCredentialsStoreMode,
    pub(crate) zed_config: AgentClientProtocolZedConfig,
    pub(crate) tools_config: ToolsConfig,
    pub(crate) commands_config: CommandsConfig,
    pub(crate) custom_providers: &'a [CustomProviderConfig],
    pub(crate) provider_timeouts: TimeoutsConfig,
    pub(crate) system_prompt: String,
    pub(crate) title: Option<String>,
    pub(crate) primary_agents: PrimaryAgentCatalog,
    pub(crate) skip_confirmations: bool,
    pub(crate) vt_config: Option<&'a VTCodeConfig>,
    pub(crate) audit_logger: Option<Arc<AcpAuditLogger>>,
}

impl ZedAgent {
    pub(crate) async fn new(mut setup: ZedAgentSetup<'_>) -> Self {
        if let Ok(workspace_root) = vtcode_commons::canonicalize(&setup.config.workspace) {
            setup.config.workspace = workspace_root;
        }
        let tool_loop_limit = setup.tools_config.max_tool_loops;
        let tool_call_delay = setup.tools_config.tool_call_delay();
        let workspace_runtime_config = SessionWorkspaceRuntimeConfig {
            zed_config: setup.zed_config.clone(),
            tools_config: setup.tools_config.clone(),
            commands_config: setup.commands_config.clone(),
            custom_providers: setup.custom_providers.to_vec(),
            skip_confirmations: setup.skip_confirmations,
        };
        let launch_runtime = Arc::new(build_launch_workspace_runtime(&setup).await);
        let provider_metrics = launch_runtime.local_tool_registry.metrics_collector();
        let mut workspace_runtimes = HashMap::new();
        if let Ok(canonical_workspace) = vtcode_commons::canonicalize(&launch_runtime.workspace_root) {
            let runtime_cell = Arc::new(OnceCell::new());
            if runtime_cell.set(Arc::clone(&launch_runtime)).is_ok() {
                drop(workspace_runtimes.insert(canonical_workspace, runtime_cell));
            }
        }
        Self {
            config: setup.config,
            credential_storage_mode: setup.credential_storage_mode,
            system_prompt: setup.system_prompt,
            sessions: Arc::new(Mutex::new(HashMap::with_capacity(10))),
            next_session_id: AtomicU64::new(0),
            acp_tool_registry: Arc::clone(&launch_runtime.acp_tool_registry),
            permission_prompter: Arc::clone(&launch_runtime.permission_prompter),
            local_tool_registry: launch_runtime.local_tool_registry.clone(),
            file_ops_tool: launch_runtime.file_ops_tool.clone(),
            thread_manager: ThreadManager::new(),
            client_capabilities: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            title: setup.title,
            primary_agents: setup.primary_agents,
            tool_loop_limit,
            tool_call_delay,
            provider_runtime: ProviderRuntimeRegistry::new(
                setup.custom_providers,
                &setup.provider_timeouts,
                provider_metrics,
            ),
            provider_timeouts: setup.provider_timeouts,
            vt_config: setup.vt_config.cloned().map(Box::new),
            audit_logger: setup.audit_logger,
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
