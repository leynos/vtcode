//! Closed-set outcomes reported for optional ACP component setup.
//!
//! Categories are assigned at the point where a stage failed, so diagnostics
//! never have to inspect arbitrary provider or command text to classify an
//! outcome. The originating error is kept where a caller genuinely needs it,
//! but it is never rendered into default diagnostics.

use super::OPTIONAL_SETUP_OPERATION;
use tracing::{debug, warn};

#[derive(Debug, PartialEq, Eq)]
pub(in crate::zed::agent) enum OptionalSetupOutcome {
    DisabledByConfiguration,
    DisabledByPolicy,
    Attached,
    AttachedWithDegradation(Vec<OptionalSetupFailure>),
    Failed(OptionalSetupFailure),
}

/// Closed-set categories for an optional integration that did not attach
/// cleanly.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::zed::agent) enum OptionalSetupFailure {
    /// The sub-agent controller rejected its configuration or failed to construct.
    SubagentControllerUnavailable,
    /// Configured background sub-agents could not be restored.
    BackgroundSubagentRestoreFailed,
    /// Ownerless background records could not be migrated to the resumed
    /// session that launched them.
    BackgroundOwnerMigrationFailed,
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
    fn category(&self) -> &'static str {
        match self {
            Self::SubagentControllerUnavailable => "subagent_controller_unavailable",
            Self::BackgroundSubagentRestoreFailed => "background_subagent_restore_failed",
            Self::BackgroundOwnerMigrationFailed => "background_owner_migration_failed",
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
pub(in crate::zed::agent) struct OptionalWorkspaceSetupOutcomes {
    pub(in crate::zed::agent) subagent_controller: OptionalSetupOutcome,
    pub(in crate::zed::agent) mcp_client: OptionalSetupOutcome,
}

impl OptionalSetupOutcome {
    pub(in crate::zed::agent) fn attached_with_degradation(failures: Vec<OptionalSetupFailure>) -> Self {
        if failures.is_empty() {
            Self::Attached
        } else {
            Self::AttachedWithDegradation(failures)
        }
    }
}

pub(in crate::zed::agent) fn report_optional_setup_outcome(component: &'static str, outcome: &OptionalSetupOutcome) {
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

pub(in crate::zed::agent) fn report_optional_workspace_setup_outcomes(outcomes: &OptionalWorkspaceSetupOutcomes) {
    report_optional_setup_outcome("subagent controller", &outcomes.subagent_controller);
    report_optional_setup_outcome("MCP client", &outcomes.mcp_client);
}
