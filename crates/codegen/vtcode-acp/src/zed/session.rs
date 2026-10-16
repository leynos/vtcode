//! Top-level glue: wire the [`ZedAgent`] into an SACP `AgentToClient`
//! connection over stdio.

use crate::acp;
use crate::audit::{AcpAuditLogger, initialize_acp_audit_logger};
use crate::register_acp_connection;
use crate::workspace::{DefaultWorkspaceTrustSynchronizer, WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use crate::zed::agent::handlers::install_handlers;
use crate::zed::agent::{ZedAgent, ZedAgentSetup};
use crate::zed::connection::ConnectionHandle;
use agent_client_protocol::{Agent, Client, ConnectionTo, Stdio};
use anyhow::{Context, Result};
use std::error::Error as StdError;
use std::fmt;
use std::future::pending;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};
use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
use vtcode_core::config::VTCodeConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::hooks::SessionEndReason;
use vtcode_core::llm::factory::register_custom_providers;
use vtcode_core::prompts::system::generate_system_instruction_with_config;

use super::AcpWorkspacePolicy;
use super::constants::{
    WORKSPACE_TRUST_ALREADY_SATISFIED_LOG, WORKSPACE_TRUST_DOWNGRADE_SKIPPED_LOG, WORKSPACE_TRUST_UPGRADE_LOG,
};
use super::helpers::PrimaryAgentCatalog;

/// Canonical, existing workspace selected by an ACP session client.
///
/// Construction validates the client-provided cwd once so session ownership,
/// trust synchronization, and workspace runtime selection share the same path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcpSessionWorkspace {
    root: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AcpWorkspaceErrorCategory {
    InvalidPath,
    ResolveFailed,
    InspectFailed,
    NotDirectory,
    UnsupportedScope,
    OutsideAllowedRoots,
}

impl AcpWorkspaceErrorCategory {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::ResolveFailed => "resolve_failed",
            Self::InspectFailed => "inspect_failed",
            Self::NotDirectory => "not_directory",
            Self::UnsupportedScope => "unsupported_scope",
            Self::OutsideAllowedRoots => "outside_allowed_roots",
        }
    }

    const fn public_message(self) -> &'static str {
        match self {
            Self::InvalidPath => "ACP session workspace must be an absolute path",
            Self::ResolveFailed => "Unable to resolve ACP session workspace",
            Self::InspectFailed => "Unable to inspect ACP session workspace",
            Self::NotDirectory => "ACP session workspace is not a directory",
            Self::UnsupportedScope => "ACP session workspace has an unsupported scope",
            Self::OutsideAllowedRoots => "ACP session workspace is outside configured allowed roots",
        }
    }
}

#[derive(Debug)]
pub(crate) struct AcpWorkspaceError {
    operation: &'static str,
    category: AcpWorkspaceErrorCategory,
    source: Option<anyhow::Error>,
}

impl AcpWorkspaceError {
    pub(crate) fn new(operation: &'static str, category: AcpWorkspaceErrorCategory) -> Self {
        Self { operation, category, source: None }
    }

    pub(crate) fn with_source(
        operation: &'static str,
        category: AcpWorkspaceErrorCategory,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self { operation, category, source: Some(source.into()) }
    }
}

impl fmt::Display for AcpWorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ACP workspace {} failed ({})", self.operation, self.category.as_str())
    }
}

impl StdError for AcpWorkspaceError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

impl From<AcpWorkspaceError> for acp::Error {
    fn from(error: AcpWorkspaceError) -> Self {
        tracing::warn!(
            operation = error.operation,
            error_category = error.category.as_str(),
            "ACP workspace request rejected"
        );
        acp::Error::invalid_params().data(serde_json::json!({
            "operation": error.operation,
            "error_category": error.category.as_str(),
            "message": error.category.public_message(),
        }))
    }
}

impl AcpSessionWorkspace {
    pub(crate) async fn from_client_cwd(requested_workspace: &Path) -> Result<Self, AcpWorkspaceError> {
        if !requested_workspace.is_absolute() {
            return Err(AcpWorkspaceError::new("resolve_session_workspace", AcpWorkspaceErrorCategory::InvalidPath));
        }
        let root = vtcode_commons::fs::canonicalize_with_context_async(requested_workspace, "ACP session cwd")
            .await
            .map_err(|error| {
                AcpWorkspaceError::with_source(
                    "resolve_session_workspace",
                    AcpWorkspaceErrorCategory::ResolveFailed,
                    error,
                )
            })?;
        let metadata = tokio::fs::metadata(&root).await.map_err(|error| {
            AcpWorkspaceError::with_source("inspect_session_workspace", AcpWorkspaceErrorCategory::InspectFailed, error)
        })?;
        if !metadata.is_dir() {
            return Err(AcpWorkspaceError::new("inspect_session_workspace", AcpWorkspaceErrorCategory::NotDirectory));
        }
        Ok(Self { root })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}

pub(super) struct AcpAgentStartup {
    config: CoreAgentConfig,
    vt_config: VTCodeConfig,
    title: Option<String>,
    skip_confirmations: bool,
    pub(super) audit_logger: Option<Arc<AcpAuditLogger>>,
    system_prompt: String,
    primary_agents: PrimaryAgentCatalog,
}

/// Runs the ACP agent over stdio using the supplied core and VT Code config.
///
/// This registers configured custom providers, applies session-history settings,
/// synchronizes workspace trust, initializes the audit logger and creates the
/// `ZedAgent` that owns the request handlers for the connection. When the
/// connection ends, SessionEnd hooks run with the matching exit or error reason;
/// shutdown hook execution is limited to three seconds. The local agent and
/// stdio connection are owned by this call for the connection lifetime.
///
/// Returns setup, connection or shutdown errors. In particular, audit logger
/// initialization, workspace trust synchronization, primary-agent discovery
/// and ACP tool-registry configuration can fail before the connection starts.
/// A connection failure is returned.
/// After a successful connection, individual SessionEnd hook command failures
/// are logged; session-state errors and shutdown timeouts are returned.
pub async fn run_acp_agent(
    config: &CoreAgentConfig,
    vt_cfg: &VTCodeConfig,
    title: Option<String>,
    skip_confirmations: bool,
) -> Result<()> {
    let startup = prepare_acp_agent_startup(config, vt_cfg, title, skip_confirmations).await?;
    run_acp_agent_runtime(startup).await
}

pub(super) async fn prepare_acp_agent_startup(
    config: &CoreAgentConfig,
    vt_cfg: &VTCodeConfig,
    title: Option<String>,
    skip_confirmations: bool,
) -> Result<AcpAgentStartup> {
    register_acp_custom_providers(vt_cfg);
    let audit_logger = initialize_acp_audit_logger(vt_cfg).await?;
    vtcode_core::utils::session_archive::apply_session_history_config_from_vtcode(vt_cfg);

    synchronize_acp_workspace_trust(config, vt_cfg).await?;

    let content = generate_system_instruction_with_config(&Default::default(), &config.workspace, Some(vt_cfg)).await;
    let system_prompt = content
        .parts
        .first()
        .and_then(|part| part.as_text())
        .map_or_else(String::new, ToString::to_string);
    let discovered = discover_subagents(&SubagentDiscoveryInput::new(config.workspace.clone()))
        .context("Failed to discover primary agents for ACP bridge")?;
    let primary_agents =
        PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, &vt_cfg.default_primary_agent);

    Ok(AcpAgentStartup {
        config: config.clone(),
        vt_config: vt_cfg.clone(),
        title,
        skip_confirmations,
        audit_logger,
        system_prompt,
        primary_agents,
    })
}

async fn synchronize_acp_workspace_trust(config: &CoreAgentConfig, vt_cfg: &VTCodeConfig) -> Result<()> {
    let zed_config = &vt_cfg.acp.zed;
    let workspace_policy = AcpWorkspacePolicy::new(&config.workspace, &zed_config.allowed_workspace_roots).await?;
    let desired_trust_level = zed_config.workspace_trust.to_workspace_trust_level();
    let trust_synchronizer = DefaultWorkspaceTrustSynchronizer::new();
    match trust_synchronizer
        .synchronize(workspace_policy.launch_context(), desired_trust_level)
        .await
        .context("Failed to synchronize workspace trust for ACP bridge")?
    {
        WorkspaceTrustSyncOutcome::Upgraded { previous, new } => {
            info!(previous = ?previous, new = ?new, "{}", WORKSPACE_TRUST_UPGRADE_LOG);
        }
        WorkspaceTrustSyncOutcome::AlreadyMatches(level) => {
            info!(level = ?level, "{}", WORKSPACE_TRUST_ALREADY_SATISFIED_LOG);
        }
        WorkspaceTrustSyncOutcome::SkippedDowngrade(current) => {
            info!(
                current = ?current,
                requested = ?zed_config.workspace_trust,
                "{}",
                WORKSPACE_TRUST_DOWNGRADE_SKIPPED_LOG
            );
        }
    }

    Ok(())
}

async fn run_acp_agent_runtime(startup: AcpAgentStartup) -> Result<()> {
    let result = tokio::task::LocalSet::new()
        .run_until(Box::pin(run_local_acp_agent(startup)))
        .await;

    if let Err(error) = result {
        error!(%error, "ACP bridge task failed");
        return Err(error);
    }
    Ok(())
}

async fn run_local_acp_agent(startup: AcpAgentStartup) -> Result<()> {
    let agent = Arc::new(create_acp_agent(startup).await?);

    run_acp_connection(agent).await
}

pub(super) async fn create_acp_agent(startup: AcpAgentStartup) -> Result<ZedAgent> {
    let AcpAgentStartup {
        config,
        vt_config,
        title,
        skip_confirmations,
        audit_logger,
        system_prompt,
        primary_agents,
    } = startup;
    Box::pin(ZedAgent::new(ZedAgentSetup {
        config,
        credential_storage_mode: vt_config.agent.credential_storage_mode,
        zed_config: vt_config.acp.zed.clone(),
        tools_config: vt_config.tools.clone(),
        commands_config: vt_config.commands.clone(),
        custom_providers: &vt_config.custom_providers,
        provider_timeouts: vt_config.timeouts.clone(),
        system_prompt,
        title,
        primary_agents,
        skip_confirmations,
        vt_config: Some(&vt_config),
        audit_logger,
    }))
    .await
}

async fn run_acp_connection(agent: Arc<ZedAgent>) -> Result<()> {
    // Build the SACP agent-side connection, then attach the vtcode request and
    // notification handlers around `ZedAgent`.
    let builder = install_handlers(Agent.builder().name("vtcode"), Arc::clone(&agent));

    // The SACP dispatch loop never exposes `ConnectionTo` outside handler
    // closures, so capture it with `connect_with`. Parking on `pending()` keeps
    // the spawned task alive for the connection lifetime.
    let attach_agent = Arc::clone(&agent);
    let connection_result = builder
        .connect_with(Stdio::new(), async move |cx: ConnectionTo<Client>| {
            let handle = ConnectionHandle::new(cx);
            if let Err(existing) = register_acp_connection(Arc::clone(&handle)) {
                warn!("ACP client already registered; continuing with existing instance");
                drop(existing);
            }
            attach_agent.attach_client(Arc::clone(&handle));
            drop(pending::<agent_client_protocol::Result<()>>().await);
            Ok(())
        })
        .await
        .map_err(|error| anyhow::anyhow!("ACP stdio connection failed: {error}"));

    finish_acp_session(agent, connection_result).await
}

async fn finish_acp_session(agent: Arc<ZedAgent>, connection_result: Result<()>) -> Result<()> {
    let end_reason = if connection_result.is_ok() {
        SessionEndReason::Exit
    } else {
        SessionEndReason::Error
    };
    let cleanup_task = tokio::spawn(async move { agent.close_all_sessions(end_reason).await });
    let cleanup_result = match tokio::time::timeout(std::time::Duration::from_secs(3), cleanup_task).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_error)) => Err(anyhow::anyhow!("ACP session cleanup task failed: {join_error}")),
        Err(timeout_error) => Err(anyhow::anyhow!("ACP session cleanup exceeded shutdown timeout: {timeout_error}")),
    };

    preserve_connection_error(connection_result, cleanup_result)
}

fn preserve_connection_error(connection_result: Result<()>, cleanup_result: Result<()>) -> Result<()> {
    connection_result?;
    cleanup_result
}

fn register_acp_custom_providers(vt_cfg: &VTCodeConfig) {
    register_custom_providers(&vt_cfg.custom_providers);
}

#[cfg(test)]
mod tests {
    use vtcode_config::core::CustomProviderConfig;
    use vtcode_core::llm::factory::{ProviderConfig, create_provider_with_config};

    use super::*;

    #[test]
    fn connection_error_takes_precedence_over_cleanup_error() {
        let result = preserve_connection_error(
            Err(anyhow::anyhow!("ACP stdio connection failed: closed")),
            Err(anyhow::anyhow!("ACP session cleanup timed out")),
        );

        assert_eq!(result.unwrap_err().to_string(), "ACP stdio connection failed: closed");
    }

    #[test]
    fn acp_launch_registers_runtime_custom_providers() {
        let mut vt_cfg = VTCodeConfig::default();
        vt_cfg.custom_providers.push(CustomProviderConfig {
            name: "acp-test-provider".to_string(),
            display_name: "ACP Test Provider".to_string(),
            base_url: "https://example.invalid/v1".to_string(),
            api_key_env: "ACP_TEST_API_KEY".to_string(),
            model: "test-model".to_string(),
            ..CustomProviderConfig::default()
        });

        register_acp_custom_providers(&vt_cfg);

        let _provider = create_provider_with_config(
            "acp-test-provider",
            ProviderConfig {
                api_key: Some("dummy-key".to_string()),
                openai_chatgpt_auth: None,
                copilot_auth: None,
                base_url: None,
                model: Some("test-model".to_string()),
                prompt_cache: None,
                timeouts: None,
                openai: None,
                anthropic: None,
                model_behaviour: None,
                workspace_root: None,
            },
        )
        .expect("ACP launch must register custom providers from runtime config");

        register_custom_providers(&[]);
    }
}
