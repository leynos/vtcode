//! Optional integrations attached to an ACP workspace tool registry.
use crate::zed::metrics::AcpMcpStartupMetricsObserver;
use std::sync::Arc;
use tracing::{debug, warn};
use vtcode_config::VTCodeConfig;
use vtcode_config::core::CustomProviderConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::mcp::plugin_providers::discover_plugin_mcp_providers;
use vtcode_core::mcp::{McpClient, McpSandboxContext, validate_mcp_config};
use vtcode_core::subagents::{SubagentController, SubagentControllerConfig};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;
use vtcode_core::tools::registry::sandbox_policy_from_runtime_config;

pub(super) struct AcpSubagentControllerSetup<'a> {
    pub(super) registry: &'a CoreToolRegistry,
    pub(super) agent_config: &'a CoreAgentConfig,
    pub(super) custom_providers: &'a [CustomProviderConfig],
    pub(super) vt_config: Option<&'a VTCodeConfig>,
    pub(super) workspace_gated: bool,
}

pub(super) fn effective_acp_subagent_concurrency(configured: usize, max_in_flight: Option<usize>) -> Option<usize> {
    match max_in_flight {
        Some(0 | 1) => None,
        Some(limit) => Some(configured.min(limit - 1)),
        None => Some(configured),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum OptionalSetupOutcome {
    DisabledByConfiguration,
    DisabledByPolicy,
    Attached,
    AttachedWithDegradation(Vec<String>),
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct OptionalWorkspaceSetupOutcomes {
    pub(super) subagent_controller: OptionalSetupOutcome,
    pub(super) mcp_client: OptionalSetupOutcome,
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

pub(super) fn report_optional_setup_outcome(component: &'static str, outcome: &OptionalSetupOutcome) {
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

pub(super) fn report_optional_workspace_setup_outcomes(outcomes: &OptionalWorkspaceSetupOutcomes) {
    report_optional_setup_outcome("subagent controller", &outcomes.subagent_controller);
    report_optional_setup_outcome("MCP client", &outcomes.mcp_client);
}

pub(super) async fn attach_acp_subagent_controller(setup: AcpSubagentControllerSetup<'_>) -> OptionalSetupOutcome {
    let AcpSubagentControllerSetup {
        registry,
        agent_config,
        custom_providers,
        vt_config,
        workspace_gated,
    } = setup;
    let controller_vt_cfg = match configure_acp_subagents(vt_config, agent_config, custom_providers) {
        AcpSubagentConfiguration::DisabledByConfiguration => return OptionalSetupOutcome::DisabledByConfiguration,
        AcpSubagentConfiguration::DisabledByPolicy => return OptionalSetupOutcome::DisabledByPolicy,
        AcpSubagentConfiguration::Ready(config) => *config,
    };

    let controller_config = SubagentControllerConfig {
        workspace_root: agent_config.workspace.clone(),
        parent_session_id: "vtcode-acp".to_string(),
        parent_model: agent_config.model.clone(),
        parent_provider: agent_config.provider.clone(),
        parent_reasoning_effort: agent_config.reasoning_effort,
        api_key: agent_config.api_key.clone(),
        vt_cfg: controller_vt_cfg.clone(),
        openai_chatgpt_auth: agent_config.openai_chatgpt_auth.clone(),
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
pub(super) async fn attach_acp_mcp_client(
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
    client.set_startup_observer(Arc::new(AcpMcpStartupMetricsObserver::new(registry.metrics_collector())));
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

pub(super) fn effective_acp_mcp_config(
    config: &VTCodeConfig,
    plugin_providers: impl IntoIterator<Item = vtcode_config::mcp::McpProviderConfig>,
) -> vtcode_config::mcp::McpClientConfig {
    let mut mcp_config = config.mcp.clone();
    mcp_config.providers.extend(plugin_providers);
    mcp_config
}

pub(super) fn configure_acp_tool_call_limits(registry: &CoreToolRegistry, vt_cfg: Option<&VTCodeConfig>) {
    let Some(harness) = vt_cfg.map(|config| &config.agent.harness) else {
        return;
    };
    let safety_gateway = registry.safety_gateway();
    let max_per_session = harness
        .max_tool_calls_per_session
        .unwrap_or_else(|| safety_gateway.max_per_session());
    safety_gateway.set_limits(harness.max_tool_calls_per_turn, max_per_session);
}
