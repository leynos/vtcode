//! Optional integrations attached to an ACP workspace tool registry.
use super::log_suppressed_error;
use diapason_config::DiapasonConfig;
use diapason_config::core::CustomProviderConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::subagents::{SubagentController, SubagentControllerConfig};
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use futures::future::BoxFuture;
use std::sync::Arc;
use tracing::{debug, warn};

mod mcp;
pub(super) use mcp::{
    AcpMcpClientAttacher, DefaultAcpMcpClientAttacher, attach_acp_mcp_client, effective_acp_mcp_config,
};

/// Operation label for every optional-component log event.
const OPTIONAL_SETUP_OPERATION: &str = "optional_component_setup";

pub(super) struct AcpSubagentControllerSetup<'a> {
    pub(super) owner_session_id: Option<&'a str>,
    pub(super) registry: &'a CoreToolRegistry,
    pub(super) agent_config: &'a CoreAgentConfig,
    pub(super) custom_providers: &'a [CustomProviderConfig],
    pub(super) vt_config: Option<&'a DiapasonConfig>,
    pub(super) workspace_gated: bool,
    /// Builds the controller without restoring its background sub-agents, so
    /// a caller can run a durability checkpoint before any child is launched.
    /// Only meaningful for an owner-scoped controller.
    pub(super) defer_background_restore: bool,
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
    Ready(Box<DiapasonConfig>),
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
        owner_session_id,
        registry,
        agent_config,
        custom_providers,
        vt_config,
        workspace_gated,
        defer_background_restore,
    } = setup;
    let controller_vt_cfg = match configure_acp_subagents(vt_config, agent_config, custom_providers) {
        AcpSubagentConfiguration::DisabledByConfiguration => return OptionalSetupOutcome::DisabledByConfiguration,
        AcpSubagentConfiguration::DisabledByPolicy => return OptionalSetupOutcome::DisabledByPolicy,
        AcpSubagentConfiguration::Ready(config) => *config,
    };

    let controller_config = SubagentControllerConfig {
        workspace_root: agent_config.workspace.clone(),
        parent_session_id: owner_session_id.unwrap_or("diapason-acp").to_string(),
        parent_model: agent_config.model.clone(),
        parent_provider: agent_config.provider.clone(),
        parent_reasoning_effort: agent_config.reasoning_effort,
        api_key: agent_config.api_key.clone(),
        vt_cfg: controller_vt_cfg.clone(),
        openai_chatgpt_auth: agent_config.openai_chatgpt_auth.clone(),
        depth: 0,
        workspace_gated,
        exec_sessions: registry.exec_session_manager(),
        output_reader: None,
        pty_manager: registry.pty_manager().clone(),
        managed_background_runtime: false,
        lock_scope: None,
    };
    let controller = match SubagentController::new_with_background_owner(controller_config, owner_session_id).await {
        Ok(controller) => Arc::new(controller),
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "subagent_controller_unavailable");
            return OptionalSetupOutcome::Failed(OptionalSetupFailure::SubagentControllerUnavailable);
        }
    };
    let failures = if defer_background_restore {
        Vec::new()
    } else {
        match owner_session_id {
            Some(_) => restore_configured_background_subagents(&controller, &controller_vt_cfg).await,
            // A legacy record has no owner, so no session controller may adopt
            // it. The workspace controller is therefore the only scope that
            // can bring it back, and it restores only those ownerless records.
            None => restore_legacy_background_subagents(&controller, &controller_vt_cfg).await,
        }
    };
    registry.set_subagent_controller(controller);
    OptionalSetupOutcome::attached_with_degradation(failures)
}

/// Restores the background sub-agents an already-built session controller
/// owns.
///
/// A resumed session defers this until its required history checkpoint has
/// succeeded, because a restored child can outlive a failed attach: the
/// fail-closed path releases the workspace lease, but shutdown aborts only
/// tracked children, never restored background children.
pub(super) async fn restore_acp_session_background_subagents(
    controller: &SubagentController,
    vt_config: Option<&DiapasonConfig>,
) -> OptionalSetupOutcome {
    let Some(config) = vt_config.filter(|config| config.subagents.enabled) else {
        return OptionalSetupOutcome::DisabledByConfiguration;
    };
    let failures = restore_configured_background_subagents(controller, config).await;
    OptionalSetupOutcome::attached_with_degradation(failures)
}

fn configure_acp_subagents(
    vt_cfg: Option<&DiapasonConfig>,
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
    config: &DiapasonConfig,
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

/// Restores background records that predate session ownership.
///
/// A session controller must not adopt an ownerless record, so the workspace
/// controller is the only scope that can restore it. Only ownerless records
/// are restored, so a record another session owns is never respawned here.
async fn restore_legacy_background_subagents(
    controller: &SubagentController,
    config: &DiapasonConfig,
) -> Vec<OptionalSetupFailure> {
    if !config.subagents.background.auto_restore {
        return Vec::new();
    }
    match controller.restore_legacy_background_subagents().await {
        Ok(_) => Vec::new(),
        Err(error) => {
            log_suppressed_error(&error, OPTIONAL_SETUP_OPERATION, "legacy_background_subagent_restore_failed");
            vec![OptionalSetupFailure::BackgroundSubagentRestoreFailed]
        }
    }
}

pub(super) fn configure_acp_tool_call_limits(registry: &CoreToolRegistry, vt_cfg: Option<&DiapasonConfig>) {
    let Some(harness) = vt_cfg.map(|config| &config.agent.harness) else {
        return;
    };
    let safety_gateway = registry.safety_gateway();
    let max_per_session = harness
        .max_tool_calls_per_session
        .unwrap_or_else(|| safety_gateway.max_per_session());
    safety_gateway.set_limits(harness.max_tool_calls_per_turn, max_per_session);
}

#[cfg(test)]
#[path = "optional_setup/tests.rs"]
mod tests;
