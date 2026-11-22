//! Optional integrations attached to an ACP workspace tool registry.
use super::log_suppressed_error;
use crate::zed::metrics::AcpMcpStartupMetricsObserver;
use diapason_config::DiapasonConfig;
use diapason_core::mcp::plugin_providers::discover_plugin_mcp_providers;
use diapason_core::mcp::{McpClient, McpSandboxContext, validate_mcp_config};
use diapason_core::sandboxing::SandboxPolicy;
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use diapason_core::tools::registry::sandbox_policy_from_runtime_config;
use futures::future::BoxFuture;
use std::sync::Arc;

mod background_restore;
mod outcomes;
mod subagents;

pub(in crate::zed::agent) use outcomes::{
    OptionalSetupFailure, OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes, report_optional_setup_outcome,
    report_optional_workspace_setup_outcomes,
};
pub(in crate::zed::agent) use subagents::{
    AcpSubagentControllerAttacher, AcpSubagentControllerSetup, DefaultAcpSubagentControllerAttacher,
    attach_acp_subagent_controller, effective_acp_subagent_concurrency, migrate_resumed_session_background_owners,
    restore_acp_session_background_subagents,
};

/// Operation label for every optional-component log event.
const OPTIONAL_SETUP_OPERATION: &str = "optional_component_setup";

/// Driven boundary for attaching the optional ACP MCP client.
pub(super) trait AcpMcpClientAttacher: Send + Sync {
    fn attach<'a>(
        &'a self,
        registry: &'a CoreToolRegistry,
        vt_config: Option<&'a DiapasonConfig>,
        workspace_root: &'a std::path::Path,
    ) -> BoxFuture<'a, OptionalSetupOutcome>;
}

/// Default adapter that attaches the configured ACP MCP client.
pub(super) struct DefaultAcpMcpClientAttacher;

impl AcpMcpClientAttacher for DefaultAcpMcpClientAttacher {
    fn attach<'a>(
        &'a self,
        registry: &'a CoreToolRegistry,
        vt_config: Option<&'a DiapasonConfig>,
        workspace_root: &'a std::path::Path,
    ) -> BoxFuture<'a, OptionalSetupOutcome> {
        Box::pin(attach_acp_mcp_client(registry, vt_config, workspace_root))
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
    vt_cfg: Option<&DiapasonConfig>,
    workspace_root: &std::path::Path,
) -> OptionalSetupOutcome {
    attach_acp_mcp_client_with(registry, vt_cfg, workspace_root, &DefaultMcpSandboxPolicy).await
}

/// Builds the sandbox context that MCP stdio providers launch under.
pub(super) trait McpSandboxPolicySource: Send + Sync {
    fn build(
        &self,
        config: &DiapasonConfig,
        workspace_root: &std::path::Path,
    ) -> anyhow::Result<Option<McpSandboxContext>>;
}

/// Default adapter over the shared sandbox-policy builder.
pub(super) struct DefaultMcpSandboxPolicy;

impl McpSandboxPolicySource for DefaultMcpSandboxPolicy {
    fn build(
        &self,
        config: &DiapasonConfig,
        workspace_root: &std::path::Path,
    ) -> anyhow::Result<Option<McpSandboxContext>> {
        acp_mcp_sandbox_context(config, workspace_root)
    }
}

async fn attach_acp_mcp_client_with(
    registry: &CoreToolRegistry,
    vt_cfg: Option<&DiapasonConfig>,
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
    config: diapason_config::mcp::McpClientConfig,
    sandbox_context: Option<McpSandboxContext>,
    enabled_provider_count: usize,
    failures: Vec<OptionalSetupFailure>,
}

async fn prepare_acp_mcp_setup(
    config: &DiapasonConfig,
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
) -> (Vec<diapason_config::mcp::McpProviderConfig>, Vec<OptionalSetupFailure>) {
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
    config: &DiapasonConfig,
    workspace_root: &std::path::Path,
) -> anyhow::Result<Option<McpSandboxContext>> {
    if config.sandbox.enabled
        && !matches!(config.sandbox.default_policy, diapason_config::SandboxPolicy::DangerFullAccess)
    {
        // Never fall back to an unsandboxed stdio launch when policy construction fails.
        let policy = sandbox_policy_from_runtime_config(&config.sandbox, workspace_root)?;
        Ok(Some(McpSandboxContext::new(policy, workspace_root)))
    } else {
        Ok(None)
    }
}

pub(super) fn effective_acp_mcp_config(
    config: &DiapasonConfig,
    plugin_providers: impl IntoIterator<Item = diapason_config::mcp::McpProviderConfig>,
) -> diapason_config::mcp::McpClientConfig {
    let mut mcp_config = config.mcp.clone();
    mcp_config.providers.extend(plugin_providers);
    mcp_config
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
mod tests {
    use super::{
        McpSandboxPolicySource, OptionalSetupFailure, OptionalSetupOutcome, attach_acp_mcp_client_with,
        report_optional_setup_outcome,
    };
    use crate::zed::agent::test_log_capture::{capturing, capturing_async};
    use assert_fs::TempDir;
    use diapason_config::DiapasonConfig;
    use diapason_core::mcp::McpSandboxContext;
    use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
    use std::path::Path;
    use tracing_subscriber::filter::LevelFilter;

    /// Sandbox-policy construction that always fails with caller-supplied text,
    /// standing in for a policy builder that cannot read its workspace.
    struct FailingSandboxPolicy(&'static str);

    impl McpSandboxPolicySource for FailingSandboxPolicy {
        fn build(&self, _config: &DiapasonConfig, _workspace_root: &Path) -> anyhow::Result<Option<McpSandboxContext>> {
            anyhow::bail!("sandbox policy build failed for {}", self.0)
        }
    }

    /// A policy that cannot be constructed must fail the component closed: no
    /// MCP client is attached, no unsandboxed fallback is started, and the
    /// originating detail never reaches default diagnostics.
    #[tokio::test]
    async fn sandbox_policy_failure_fails_closed_without_raw_detail() {
        let temp = TempDir::new().expect("temporary workspace");
        let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
        let mut vt_cfg = DiapasonConfig::default();
        vt_cfg.mcp.enabled = true;
        let sentinel = "private-sentinel-sandbox-policy";

        let (outcome, warnings) = capturing_async(
            LevelFilter::INFO,
            attach_acp_mcp_client_with(&registry, Some(&vt_cfg), temp.path(), &FailingSandboxPolicy(sentinel)),
        )
        .await;

        assert_eq!(
            outcome,
            OptionalSetupOutcome::Failed(OptionalSetupFailure::McpSandboxPolicyUnavailable),
            "policy construction failure must fail the component closed"
        );
        assert!(registry.mcp_client().is_none(), "the failure must happen before MCP initialization or attachment");

        let attached_warnings = warnings.join("\n");
        assert!(
            !attached_warnings.contains(sentinel),
            "originating detail must stay out of operator-visible logs, got {attached_warnings}"
        );
        assert!(
            !attached_warnings.contains("sandbox policy build failed"),
            "raw error text must stay out of operator-visible logs, got {attached_warnings}"
        );

        // The operator-facing report is emitted by the caller, so assert its
        // bounded fields on the reported category rather than on attach.
        let (_, reported) = capturing(|| report_optional_setup_outcome("MCP client", &outcome));
        let reported = reported.join("\n");
        assert!(reported.contains("operation=\"optional_component_setup\""));
        assert!(reported.contains("component=\"MCP client\""));
        assert!(reported.contains("outcome=\"failed\""));
        assert!(reported.contains("error_category=\"mcp_sandbox_policy_unavailable\""));
        assert!(
            !reported.contains(sentinel),
            "the reported category must not carry the originating detail, got {reported}"
        );
    }

    /// The suppressed chain remains at debug, which is a diagnostic aid rather
    /// than redaction.
    #[tokio::test]
    async fn sandbox_policy_failure_keeps_the_full_chain_only_at_debug() {
        let temp = TempDir::new().expect("temporary workspace");
        let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
        let mut vt_cfg = DiapasonConfig::default();
        vt_cfg.mcp.enabled = true;
        let sentinel = "private-sentinel-sandbox-debug";

        let (_, debug_events) = capturing_async(
            LevelFilter::DEBUG,
            attach_acp_mcp_client_with(&registry, Some(&vt_cfg), temp.path(), &FailingSandboxPolicy(sentinel)),
        )
        .await;

        let debugged = debug_events.join("\n");
        assert!(
            debugged.contains(sentinel) && debugged.contains("sandbox policy build failed"),
            "the debug event must retain the suppressed chain, got {debugged}"
        );
        assert!(debugged.contains("error_category=\"mcp_sandbox_policy_unavailable\""));
    }

    /// Degraded outcomes report a bounded category and count, never the joined
    /// failure text they replaced.
    #[test]
    fn degraded_outcome_reports_bounded_categories() {
        let outcome = OptionalSetupOutcome::AttachedWithDegradation(vec![
            OptionalSetupFailure::BackgroundSubagentRestoreFailed,
            OptionalSetupFailure::McpToolRegistrationFailed,
        ]);

        let (_, warnings) = capturing(|| report_optional_setup_outcome("MCP client", &outcome));

        let warned = warnings.join("\n");
        assert!(warned.contains("outcome=\"attached_with_degradation\""));
        assert!(warned.contains("failure_count=2"));
        assert!(warned.contains("error_category=\"background_subagent_restore_failed\""));
        assert!(!warned.contains("failed to restore"), "joined failure text must not return");
    }

    /// Failed outcomes publish only their stable category code.
    #[test]
    fn failed_outcome_reports_a_bounded_category() {
        let outcome = OptionalSetupOutcome::Failed(OptionalSetupFailure::McpSandboxPolicyUnavailable);

        let (_, warnings) = capturing(|| report_optional_setup_outcome("MCP client", &outcome));

        let warned = warnings.join("\n");
        assert!(warned.contains("outcome=\"failed\""));
        assert!(warned.contains("error_category=\"mcp_sandbox_policy_unavailable\""));
    }
}
