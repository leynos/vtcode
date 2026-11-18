//! MCP client setup for an ACP workspace tool registry.
//!
//! Attaching the optional MCP client is a distinct responsibility from the
//! optional sub-agent controller: it prepares provider configuration, builds
//! the sandbox context, initializes the client, and registers its tools. The
//! parent module keeps the shared outcome types and reporting.

use super::super::log_suppressed_error;
use super::{OptionalSetupFailure, OptionalSetupOutcome};
use crate::zed::metrics::AcpMcpStartupMetricsObserver;
use diapason_config::DiapasonConfig;
use diapason_core::mcp::plugin_providers::discover_plugin_mcp_providers;
use diapason_core::mcp::{McpClient, McpSandboxContext, validate_mcp_config};
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use diapason_core::tools::registry::sandbox_policy_from_runtime_config;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Driven boundary for attaching the optional ACP MCP client.
pub(in crate::zed::agent) trait AcpMcpClientAttacher: Send + Sync {
    fn attach<'a>(
        &'a self,
        registry: &'a CoreToolRegistry,
        vt_config: Option<&'a DiapasonConfig>,
        workspace_root: &'a std::path::Path,
    ) -> BoxFuture<'a, OptionalSetupOutcome>;
}

/// Default adapter that attaches the configured ACP MCP client.
pub(in crate::zed::agent) struct DefaultAcpMcpClientAttacher;

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
pub(in crate::zed::agent) async fn attach_acp_mcp_client(
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

pub(super) async fn attach_acp_mcp_client_with(
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
            log_suppressed_error(&error, super::OPTIONAL_SETUP_OPERATION, "mcp_client_initialization_failed");
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
        log_suppressed_error(&error, super::OPTIONAL_SETUP_OPERATION, "mcp_tool_registration_failed");
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
        log_suppressed_error(&error, super::OPTIONAL_SETUP_OPERATION, "mcp_configuration_invalid");
        failures.push(OptionalSetupFailure::McpConfigurationInvalid);
    }
    // A policy that cannot be constructed must fail the component closed; the
    // stdio launch is never downgraded to an unsandboxed one.
    let sandbox_context = sandbox_policy.build(config, workspace_root).map_err(|error| {
        log_suppressed_error(&error, super::OPTIONAL_SETUP_OPERATION, "mcp_sandbox_policy_unavailable");
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
            log_suppressed_error(&error, super::OPTIONAL_SETUP_OPERATION, "plugin_provider_discovery_failed");
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

pub(in crate::zed::agent) fn effective_acp_mcp_config(
    config: &DiapasonConfig,
    plugin_providers: impl IntoIterator<Item = diapason_config::mcp::McpProviderConfig>,
) -> diapason_config::mcp::McpClientConfig {
    let mut mcp_config = config.mcp.clone();
    mcp_config.providers.extend(plugin_providers);
    mcp_config
}
