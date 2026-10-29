//! Optional integrations attached to an ACP workspace tool registry.
use super::log_suppressed_error;
use crate::zed::metrics::AcpMcpStartupMetricsObserver;
use futures::future::BoxFuture;
use std::sync::Arc;
use tracing::{debug, warn};
use vtcode_config::VTCodeConfig;
use vtcode_config::core::CustomProviderConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::mcp::plugin_providers::discover_plugin_mcp_providers;
use vtcode_core::mcp::{McpClient, McpSandboxContext, validate_mcp_config};
use vtcode_core::sandboxing::SandboxPolicy;
use vtcode_core::subagents::{SubagentController, SubagentControllerConfig};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;
use vtcode_core::tools::registry::sandbox_policy_from_runtime_config;

/// Operation label for every optional-component log event.
const OPTIONAL_SETUP_OPERATION: &str = "optional_component_setup";

pub(super) struct AcpSubagentControllerSetup<'a> {
    pub(super) registry: &'a CoreToolRegistry,
    pub(super) agent_config: &'a CoreAgentConfig,
    pub(super) custom_providers: &'a [CustomProviderConfig],
    pub(super) vt_config: Option<&'a VTCodeConfig>,
    pub(super) workspace_gated: bool,
}

/// Driven boundary for attaching the optional ACP sub-agent controller.
pub(super) trait AcpSubagentControllerAttacher: Send + Sync {
    fn attach<'a>(&'a self, setup: AcpSubagentControllerSetup<'a>) -> BoxFuture<'a, OptionalSetupOutcome>;
}

/// Default adapter that attaches the configured ACP sub-agent controller.
pub(super) struct DefaultAcpSubagentControllerAttacher;

impl AcpSubagentControllerAttacher for DefaultAcpSubagentControllerAttacher {
    fn attach<'a>(&'a self, setup: AcpSubagentControllerSetup<'a>) -> BoxFuture<'a, OptionalSetupOutcome> {
        Box::pin(attach_acp_subagent_controller(setup))
    }
}

/// Driven boundary for attaching the optional ACP MCP client.
pub(super) trait AcpMcpClientAttacher: Send + Sync {
    fn attach<'a>(
        &'a self,
        registry: &'a CoreToolRegistry,
        vt_config: Option<&'a VTCodeConfig>,
        workspace_root: &'a std::path::Path,
    ) -> BoxFuture<'a, OptionalSetupOutcome>;
}

/// Default adapter that attaches the configured ACP MCP client.
pub(super) struct DefaultAcpMcpClientAttacher;

impl AcpMcpClientAttacher for DefaultAcpMcpClientAttacher {
    fn attach<'a>(
        &'a self,
        registry: &'a CoreToolRegistry,
        vt_config: Option<&'a VTCodeConfig>,
        workspace_root: &'a std::path::Path,
    ) -> BoxFuture<'a, OptionalSetupOutcome> {
        Box::pin(attach_acp_mcp_client(registry, vt_config, workspace_root))
    }
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
    AttachedWithDegradation(Vec<OptionalSetupFailure>),
    Failed(OptionalSetupFailure),
}

/// Closed-set categories for an optional integration that did not attach
/// cleanly.
///
/// Categories are assigned at the point where the stage failed, so diagnostics
/// never have to inspect arbitrary provider or command text to classify an
/// outcome. The originating error is kept where a caller genuinely needs it,
/// but it is never rendered into default diagnostics.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OptionalSetupFailure {
    /// The sub-agent controller rejected its configuration or failed to construct.
    SubagentControllerUnavailable,
    /// Configured background sub-agents could not be restored.
    BackgroundSubagentRestoreFailed,
    /// Agent Plugin MCP provider discovery did not complete.
    PluginProviderDiscoveryFailed,
    /// The effective MCP client configuration failed validation.
    McpConfigurationInvalid,
    /// The MCP sandbox policy could not be constructed. Never downgraded to an
    /// unsandboxed launch.
    McpSandboxPolicyUnavailable,
    /// The MCP client itself failed to initialize.
    McpClientInitializationFailed,
    /// Fewer MCP providers initialized than were enabled.
    McpProviderInitializationIncomplete { initialized: usize, enabled: usize },
    /// Initialized MCP tools could not be registered on the tool registry.
    McpToolRegistrationFailed,
}

impl OptionalSetupFailure {
    /// Stable category code published in diagnostics and used by tests.
    pub(super) fn category(&self) -> &'static str {
        match self {
            Self::SubagentControllerUnavailable => "subagent_controller_unavailable",
            Self::BackgroundSubagentRestoreFailed => "background_subagent_restore_failed",
            Self::PluginProviderDiscoveryFailed => "plugin_provider_discovery_failed",
            Self::McpConfigurationInvalid => "mcp_configuration_invalid",
            Self::McpSandboxPolicyUnavailable => "mcp_sandbox_policy_unavailable",
            Self::McpClientInitializationFailed => "mcp_client_initialization_failed",
            Self::McpProviderInitializationIncomplete { .. } => "mcp_provider_initialization_incomplete",
            Self::McpToolRegistrationFailed => "mcp_tool_registration_failed",
        }
    }
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
    fn attached_with_degradation(failures: Vec<OptionalSetupFailure>) -> Self {
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
            debug!(
                operation = OPTIONAL_SETUP_OPERATION,
                component,
                outcome = "disabled_by_configuration",
                "Optional ACP component is disabled by configuration"
            );
        }
        OptionalSetupOutcome::DisabledByPolicy => {
            debug!(
                operation = OPTIONAL_SETUP_OPERATION,
                component,
                outcome = "disabled_by_policy",
                "Optional ACP component is disabled by runtime policy"
            );
        }
        OptionalSetupOutcome::Attached => {
            debug!(
                operation = OPTIONAL_SETUP_OPERATION,
                component,
                outcome = "attached",
                "Optional ACP component attached successfully"
            );
        }
        OptionalSetupOutcome::AttachedWithDegradation(failures) => {
            warn!(
                operation = OPTIONAL_SETUP_OPERATION,
                component,
                outcome = "attached_with_degradation",
                failure_count = failures.len(),
                error_category = failures.first().map_or("none", OptionalSetupFailure::category),
                "Optional ACP component attached with degraded setup"
            );
        }
        OptionalSetupOutcome::Failed(failure) => {
            warn!(
                operation = OPTIONAL_SETUP_OPERATION,
                component,
                outcome = "failed",
                error_category = failure.category(),
                "Optional ACP component setup failed; continuing without it"
            );
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
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "subagent_controller_unavailable");
            return OptionalSetupOutcome::Failed(OptionalSetupFailure::SubagentControllerUnavailable);
        }
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
) -> Vec<OptionalSetupFailure> {
    if !config.subagents.background.auto_restore {
        return Vec::new();
    }
    match controller.restore_background_subagents().await {
        Ok(_) => Vec::new(),
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "background_subagent_restore_failed");
            vec![OptionalSetupFailure::BackgroundSubagentRestoreFailed]
        }
    }
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
    attach_acp_mcp_client_with(registry, vt_cfg, workspace_root, &DefaultMcpSandboxPolicy).await
}

/// Builds the sandbox context that MCP stdio providers launch under.
pub(super) trait McpSandboxPolicySource: Send + Sync {
    fn build(
        &self,
        config: &VTCodeConfig,
        workspace_root: &std::path::Path,
    ) -> anyhow::Result<Option<McpSandboxContext>>;
}

/// Default adapter over the shared sandbox-policy builder.
pub(super) struct DefaultMcpSandboxPolicy;

impl McpSandboxPolicySource for DefaultMcpSandboxPolicy {
    fn build(
        &self,
        config: &VTCodeConfig,
        workspace_root: &std::path::Path,
    ) -> anyhow::Result<Option<McpSandboxContext>> {
        acp_mcp_sandbox_context(config, workspace_root)
    }
}

async fn attach_acp_mcp_client_with(
    registry: &CoreToolRegistry,
    vt_cfg: Option<&VTCodeConfig>,
    workspace_root: &std::path::Path,
    sandbox_policy: &dyn McpSandboxPolicySource,
) -> OptionalSetupOutcome {
    let Some(config) = vt_cfg.filter(|config| config.mcp.enabled) else {
        return OptionalSetupOutcome::DisabledByConfiguration;
    };
    let setup = match prepare_acp_mcp_setup(config, workspace_root, sandbox_policy).await {
        Ok(setup) => setup,
        Err(failure) => return OptionalSetupOutcome::Failed(failure),
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
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "mcp_client_initialization_failed");
            return OptionalSetupOutcome::Failed(OptionalSetupFailure::McpClientInitializationFailed);
        }
    }
    let initialized_provider_count = client.get_status().provider_count;
    if initialized_provider_count < enabled_provider_count {
        failures.push(OptionalSetupFailure::McpProviderInitializationIncomplete {
            initialized: initialized_provider_count,
            enabled: enabled_provider_count,
        });
    }
    let client = Arc::new(client);
    registry.set_mcp_client(client).await;
    if let Err(error) = registry.refresh_mcp_tools().await {
        log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "mcp_tool_registration_failed");
        failures.push(OptionalSetupFailure::McpToolRegistrationFailed);
    }
    OptionalSetupOutcome::attached_with_degradation(failures)
}

struct AcpMcpSetupPlan {
    config: vtcode_config::mcp::McpClientConfig,
    sandbox_context: Option<McpSandboxContext>,
    enabled_provider_count: usize,
    failures: Vec<OptionalSetupFailure>,
}

async fn prepare_acp_mcp_setup(
    config: &VTCodeConfig,
    workspace_root: &std::path::Path,
    sandbox_policy: &dyn McpSandboxPolicySource,
) -> Result<AcpMcpSetupPlan, OptionalSetupFailure> {
    let (plugin_providers, mut failures) = discover_acp_plugin_mcp_providers(workspace_root).await;
    let mcp_config = effective_acp_mcp_config(config, plugin_providers);
    if let Err(error) = validate_mcp_config(&mcp_config) {
        log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "mcp_configuration_invalid");
        failures.push(OptionalSetupFailure::McpConfigurationInvalid);
    }
    // A policy that cannot be constructed must fail the component closed; the
    // stdio launch is never downgraded to an unsandboxed one.
    let sandbox_context = sandbox_policy.build(config, workspace_root).map_err(|error| {
        log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "mcp_sandbox_policy_unavailable");
        OptionalSetupFailure::McpSandboxPolicyUnavailable
    })?;
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
) -> (Vec<vtcode_config::mcp::McpProviderConfig>, Vec<OptionalSetupFailure>) {
    let plugin_workspace = workspace_root.to_path_buf();
    match tokio::task::spawn_blocking(move || discover_plugin_mcp_providers(&plugin_workspace)).await {
        Ok(providers) => (providers, Vec::new()),
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "plugin_provider_discovery_failed");
            (Vec::new(), vec![OptionalSetupFailure::PluginProviderDiscoveryFailed])
        }
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
