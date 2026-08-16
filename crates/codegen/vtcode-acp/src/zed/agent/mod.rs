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
use vtcode_config::TimeoutsConfig;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::CustomProviderConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::threads::ThreadManager;
use vtcode_core::hooks::HookMessage;
use vtcode_core::subagents::SubagentController;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

use super::helpers::PrimaryAgentCatalog;
use super::types::SessionHandle;
use crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE;

mod optional_setup;
mod session_workspace;
use optional_setup::{
    AcpMcpClientAttacher, AcpSubagentControllerAttacher, AcpSubagentControllerSetup, DefaultAcpMcpClientAttacher,
    DefaultAcpSubagentControllerAttacher, OptionalSetupFailure, OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes,
    configure_acp_tool_call_limits, effective_acp_mcp_config, effective_acp_subagent_concurrency,
    report_optional_workspace_setup_outcomes,
};
pub(super) use session_workspace::{SessionWorkspaceLease, SessionWorkspaceRuntime};
use session_workspace::{
    SessionWorkspaceRuntimeConfig, WorkspaceRuntimeCache, WorkspaceSetupPorts, build_launch_workspace_runtime,
};

fn warn_hook_message(hook_event: &'static str, message: &HookMessage) {
    tracing::warn!(
        hook_event,
        level = ?message.level,
        message_bytes = message.text.len(),
        "ACP hook emitted a message"
    );
}

fn warn_hook_block_reason(hook_event: &'static str, reason: &str) {
    tracing::warn!(hook_event, reason_bytes = reason.len(), "ACP hook blocked an operation");
}

/// Renders an error through its source chain, deferring formatting until the
/// event is actually enabled.
struct ErrorChain<'a>(&'a dyn std::fmt::Display);

impl std::fmt::Display for ErrorChain<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

/// Record the originating error for a redacted ACP failure.
///
/// ACP clients only ever see the closed-set `operation`/`error_category`
/// reason codes, because the underlying error may embed workspace paths,
/// provider detail or archive contents. That leaves the local log as the only
/// place the root cause can be recovered, so it is emitted here at `debug`
/// level, where trusted diagnostics are enabled deliberately rather than by
/// default. The client-facing payloads are unchanged.
fn log_suppressed_error(error: &impl std::fmt::Display, operation: &'static str, error_category: &'static str) {
    tracing::debug!(
        operation,
        error_category,
        error = %ErrorChain(error),
        "Suppressed ACP failure detail"
    );
}

fn warn_workspace_mcp_shutdown_failure(error: &anyhow::Error) {
    let error_chain_depth = error.chain().count();
    tracing::warn!(
        error_category = "mcp_client_shutdown",
        error_chain_depth,
        "Failed to shut down ACP workspace MCP client"
    );
}

/// Record why a session's workspace runtime could not be read and return a
/// fixed client-facing failure.
///
/// `SessionHandle::workspace_runtime` fails only when the session data lock is
/// poisoned. Its context names the runtime operation and its sources may embed
/// workload detail, so the log carries the chain depth only while the chain
/// itself stays at the `debug` level used for suppressed ACP failures.
fn report_session_runtime_failure(error: &anyhow::Error) -> String {
    log_suppressed_error(error, "session_workspace_runtime_read", "session_lock_unavailable");
    tracing::warn!(
        operation = "session_workspace_runtime_read",
        component = "session_workspace_runtime",
        outcome = "failed",
        error_category = "session_lock_unavailable",
        error_chain_depth = error.chain().count(),
        "Failed to read the ACP session workspace runtime"
    );
    SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE.to_string()
}

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
mod tool_recovery;
mod updates;

#[cfg(test)]
mod acp_logging_tests {
    use super::{log_suppressed_error, warn_hook_block_reason, warn_hook_message, warn_workspace_mcp_shutdown_failure};
    use std::fmt::Write as FmtWrite;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};
    use vtcode_core::hooks::HookMessage;

    #[derive(Clone, Default)]
    struct HookLogCapture(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for HookLogCapture
    where
        S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            let mut fields = CapturedFields::default();
            event.record(&mut fields);
            self.0.lock().expect("hook log capture lock").push(fields.0);
        }
    }

    #[derive(Default)]
    struct CapturedFields(String);

    impl Visit for CapturedFields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            write!(&mut self.0, " {}={value}", field.name()).expect("write trace field");
        }
    }

    #[test]
    fn hook_logs_keep_event_level_and_size_without_hook_text() {
        let private_path = "/home/private/workspace";
        let private_message = format!("hook stdout from {private_path}");
        let private_reason = format!("blocked because of {private_path}");
        let capture = HookLogCapture::default();
        let events = Arc::clone(&capture.0);
        let subscriber = Registry::default().with(capture);

        tracing::subscriber::with_default(subscriber, || {
            warn_hook_message("PreToolUse", &HookMessage::warning(private_message.clone()));
            warn_hook_block_reason("UserPromptSubmit", &private_reason);
            warn_workspace_mcp_shutdown_failure(&anyhow::anyhow!("shutdown failed for {private_path}"));
        });

        let events = events.lock().expect("hook log capture lock");
        assert_eq!(events.len(), 3);
        let captured = events.join("\n");
        assert!(!captured.contains(private_message.as_str()), "hook message text must stay out of logs");
        assert!(!captured.contains(private_reason.as_str()), "hook block reasons must stay out of logs");
        assert!(!captured.contains(private_path), "workspace paths must stay out of logs");
        assert!(captured.contains("hook_event=\"PreToolUse\""));
        assert!(captured.contains("level=Warning"));
        assert!(captured.contains(&format!("message_bytes={}", private_message.len())));
        assert!(captured.contains("hook_event=\"UserPromptSubmit\""));
        assert!(captured.contains(&format!("reason_bytes={}", private_reason.len())));
        assert!(captured.contains("error_category=\"mcp_client_shutdown\""));
        assert!(captured.contains("error_chain_depth=1"));
    }

    #[test]
    fn suppressed_error_detail_keeps_the_full_chain_at_debug() {
        let capture = HookLogCapture::default();
        let events = Arc::clone(&capture.0);
        let subscriber = Registry::default().with(capture);
        let error = anyhow::anyhow!("archive write failed").context("checkpointing ACP session");

        tracing::subscriber::with_default(subscriber, || {
            log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
        });

        let events = events.lock().expect("hook log capture lock");
        assert_eq!(events.len(), 1);
        let captured = &events[0];
        assert!(captured.contains("operation=\"session_checkpoint\""));
        assert!(captured.contains("error_category=\"archive_write_failed\""));
        assert!(
            captured.contains("error=checkpointing ACP session: archive write failed"),
            "the debug event must retain the source chain, got {captured}"
        );
    }

    #[test]
    fn suppressed_error_detail_stays_out_of_default_log_levels() {
        let capture = HookLogCapture::default();
        let events = Arc::clone(&capture.0);
        let subscriber = Registry::default()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(capture);
        let private_path = "/home/private/workspace";

        tracing::subscriber::with_default(subscriber, || {
            log_suppressed_error(
                &anyhow::anyhow!("failed to read {private_path}"),
                "session_list",
                "archive_read_failed",
            );
        });

        let events = events.lock().expect("hook log capture lock");
        assert!(events.is_empty(), "suppressed detail must not reach warn/error output, got {events:?}");
    }

    #[test]
    fn session_runtime_failure_reports_a_stable_reason_without_the_error_chain() {
        let capture = HookLogCapture::default();
        let events = Arc::clone(&capture.0);
        // Capture only operator-visible severities: the full chain stays at
        // debug by design, so asserting on it here would contradict the
        // suppressed-detail convention this boundary follows.
        let subscriber = Registry::default()
            .with(tracing_subscriber::filter::LevelFilter::WARN)
            .with(capture);
        let private_context = "/home/private/workspace/session-archive";
        let error =
            anyhow::anyhow!("poisoned lock for {private_context}").context("reading ACP session workspace runtime");

        let message = tracing::subscriber::with_default(subscriber, || super::report_session_runtime_failure(&error));

        assert_eq!(
            message,
            crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE,
            "the client-facing failure must be a fixed message"
        );
        assert!(!message.contains(private_context), "the failure must not echo workspace detail");

        let events = events.lock().expect("hook log capture lock");
        let captured = events.join("\n");
        assert!(!captured.contains(private_context), "workspace paths must stay out of logs");
        assert!(!captured.contains("poisoned lock for"), "raw error text must stay out of warnings");
        assert!(captured.contains("operation=\"session_workspace_runtime_read\""));
        assert!(captured.contains("component=\"session_workspace_runtime\""));
        assert!(captured.contains("outcome=\"failed\""));
        assert!(captured.contains("error_category=\"session_lock_unavailable\""));
        assert!(captured.contains("error_chain_depth=2"));
    }
}

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
    workspace_setup_ports: WorkspaceSetupPorts,
    workspace_policy: super::AcpWorkspacePolicy,
    launch_workspace_runtime: Arc<SessionWorkspaceRuntime>,
    workspace_runtimes: tokio::sync::Mutex<WorkspaceRuntimeCache<SessionWorkspaceRuntime>>,
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
    pub(crate) async fn new(mut setup: ZedAgentSetup<'_>) -> anyhow::Result<Self> {
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
        let workspace_policy =
            super::AcpWorkspacePolicy::new(&setup.config.workspace, &setup.zed_config.allowed_workspace_roots)
                .await
                .context("building ACP allowed-workspace policy")?;
        let workspace_setup_ports = WorkspaceSetupPorts::default();
        let launch_runtime = Arc::new(build_launch_workspace_runtime(&setup, &workspace_setup_ports).await?);
        let provider_metrics = launch_runtime.local_tool_registry.metrics_collector();
        let mut workspace_runtimes = WorkspaceRuntimeCache::new(Arc::clone(&provider_metrics));
        if let Ok(canonical_workspace) = vtcode_commons::canonicalize(&launch_runtime.workspace_root) {
            workspace_runtimes.remember_runtime(canonical_workspace, &launch_runtime);
        }
        Ok(Self {
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
            workspace_setup_ports,
            workspace_policy,
            launch_workspace_runtime: Arc::clone(&launch_runtime),
            workspace_runtimes: tokio::sync::Mutex::new(workspace_runtimes),
            workspace_trust_synchronizer: Arc::new(crate::workspace::DefaultWorkspaceTrustSynchronizer::new()),
        })
    }

    fn session_subagent_controller(&self, session: &SessionHandle) -> anyhow::Result<Option<Arc<SubagentController>>> {
        let workspace_runtime = session
            .workspace_runtime()
            .context("reading ACP session workspace runtime for subagent controller")?;
        Ok(match workspace_runtime {
            Some(runtime) => runtime.local_tool_registry.subagent_controller(),
            None => self.local_tool_registry.subagent_controller(),
        })
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
