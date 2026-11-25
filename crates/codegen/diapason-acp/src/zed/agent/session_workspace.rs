//! Workspace-scoped ACP registries, prompts, permissions, and runtime setup.
use anyhow::Context;
use diapason_config::core::CustomProviderConfig;
use diapason_config::{DiapasonConfig, SubagentDiscoveryInput, discover_subagents};
use diapason_core::config::ToolDocumentationMode;
use diapason_core::config::types::{AgentConfig as CoreAgentConfig, CapabilityLevel};
use diapason_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
use diapason_core::metrics::MetricsCollector;
use diapason_core::prompts::system::generate_system_instruction_with_config;
use diapason_core::tools::file_ops::FileOpsTool;
use diapason_core::tools::grep_file::GrepSearchManager;
use diapason_core::tools::handlers::{SessionSurface, SessionToolsConfig, ToolModelCapabilities};
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use futures::future::BoxFuture;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use super::super::helpers::PrimaryAgentCatalogue;
use super::super::session::AcpSessionWorkspace;
use super::{
    AcpMcpClientAttacher, AcpSubagentControllerAttacher, AcpSubagentControllerSetup, DefaultAcpMcpClientAttacher,
    DefaultAcpSubagentControllerAttacher, OptionalSetupFailure, OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes,
    ZedAgent, ZedAgentSetup, configure_acp_tool_call_limits, report_optional_workspace_setup_outcomes,
};
use crate::permissions::{AcpPermissionPrompter, DefaultPermissionPrompter};
use crate::tooling::AcpToolRegistry;

mod runtime;
use runtime::WorkspaceRuntimeLookup;
pub(super) use runtime::{
    PendingWorkspaceRestore, WorkspaceRuntimeCache, WorkspaceRuntimeLifecycle, WorkspaceRuntimeLifecycleState,
};
pub(in crate::zed) use runtime::{SessionWorkspaceLease, SessionWorkspaceRuntime};

#[derive(Clone)]
pub(super) struct SessionWorkspaceRuntimeConfig {
    pub(super) zed_config: AgentClientProtocolZedConfig,
    pub(super) tools_config: ToolsConfig,
    pub(super) commands_config: CommandsConfig,
    pub(super) custom_providers: Vec<CustomProviderConfig>,
    pub(super) skip_confirmations: bool,
}

struct SessionWorkspaceRuntimeSetup<'a> {
    base_config: &'a CoreAgentConfig,
    workspace: &'a AcpSessionWorkspace,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
    vt_config: Option<&'a DiapasonConfig>,
    /// Skips the workspace controller's restore of ownerless background
    /// records. A resumed attach always sets this, because that restore waits
    /// for the attach's required checkpoint and the workspace scope must not
    /// relaunch a row the session controller may yet adopt.
    defer_background_restore: bool,
}

struct PreparedWorkspace<'a> {
    workspace_root: PathBuf,
    session_config: CoreAgentConfig,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
    vt_config: Option<&'a DiapasonConfig>,
    system_prompt: String,
    primary_agents: PrimaryAgentCatalogue,
    workspace_hooks_gated: bool,
    file_ops_tool: Option<FileOpsTool>,
    /// Forwarded to the workspace controller so this runtime can skip
    /// restoring ownerless background records.
    defer_background_restore: bool,
}

struct AcpWorkspaceToolSurface {
    registry: Arc<AcpToolRegistry>,
    permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
}

struct InitializedWorkspaceToolRegistry {
    registry: CoreToolRegistry,
    optional_setup_outcomes: OptionalWorkspaceSetupOutcomes,
}

struct OptionalWorkspaceComponentsSetup<'a> {
    registry: &'a CoreToolRegistry,
    agent_config: &'a CoreAgentConfig,
    custom_providers: &'a [CustomProviderConfig],
    vt_config: Option<&'a DiapasonConfig>,
    workspace_gated: bool,
    workspace_root: &'a Path,
    /// See [`SessionWorkspaceRuntimeSetup::defer_background_restore`].
    defer_background_restore: bool,
}

/// Port for discovering the primary agents associated with one ACP workspace.
pub(super) trait WorkspaceAgentDiscovery: Send + Sync {
    fn discover(
        &self,
        input: &SubagentDiscoveryInput,
        default_primary_agent: &str,
    ) -> anyhow::Result<PrimaryAgentCatalogue>;
}

/// Port for creating and configuring a workspace's core tool registry.
pub(super) trait WorkspaceToolRegistrySetup: Send + Sync {
    fn initialize<'a>(
        &'a self,
        workspace_root: &'a Path,
        runtime_config: &'a SessionWorkspaceRuntimeConfig,
        vt_config: Option<&'a DiapasonConfig>,
    ) -> BoxFuture<'a, anyhow::Result<CoreToolRegistry>>;
}

/// Port for constructing the optional workspace file-operations tool.
pub(super) trait WorkspaceFileOpsToolConstruction: Send + Sync {
    fn construct(
        &self,
        workspace_root: &Path,
        zed_config: &AgentClientProtocolZedConfig,
    ) -> anyhow::Result<Option<FileOpsTool>>;
}

/// Port for projecting the core registry into the ACP model-facing tool surface.
pub(super) trait AcpWorkspaceToolProjection: Send + Sync {
    fn project<'a>(
        &'a self,
        local_tool_registry: &'a CoreToolRegistry,
        workspace_root: &'a Path,
        zed_config: &'a AgentClientProtocolZedConfig,
        tools_config: &'a ToolsConfig,
        list_files_enabled: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<AcpToolRegistry>>>;
}

/// Port for building the permission policy around the projected ACP tools.
pub(super) trait AcpPermissionPrompterConstruction: Send + Sync {
    fn construct(
        &self,
        registry: Arc<AcpToolRegistry>,
        skip_confirmations: bool,
    ) -> anyhow::Result<Arc<dyn AcpPermissionPrompter + Send + Sync>>;
}

/// The purpose-shaped adapters used at the workspace runtime composition root.
#[derive(Clone)]
pub(super) struct WorkspaceSetupPorts {
    agent_discovery: Arc<dyn WorkspaceAgentDiscovery>,
    registry_setup: Arc<dyn WorkspaceToolRegistrySetup>,
    file_ops_tool_construction: Arc<dyn WorkspaceFileOpsToolConstruction>,
    subagent_attachment: Arc<dyn AcpSubagentControllerAttacher>,
    mcp_attachment: Arc<dyn AcpMcpClientAttacher>,
    tool_projection: Arc<dyn AcpWorkspaceToolProjection>,
    permission_construction: Arc<dyn AcpPermissionPrompterConstruction>,
}

impl WorkspaceSetupPorts {
    /// Return these ports with `metrics` installed on every workspace registry
    /// created by the default registry setup.
    ///
    /// ACP workspace registries share the agent-wide collector so recovery
    /// counters recorded by any workspace appear in the agent's snapshot.
    pub(super) fn with_metrics(mut self, metrics: Arc<MetricsCollector>) -> Self {
        self.registry_setup = Arc::new(DefaultWorkspaceToolRegistrySetup { metrics: Some(metrics) });
        self
    }

    pub(super) async fn attach_session_subagent_controller(
        &self,
        setup: AcpSubagentControllerSetup<'_>,
    ) -> OptionalSetupOutcome {
        self.subagent_attachment.attach(setup).await
    }
}

impl Default for WorkspaceSetupPorts {
    fn default() -> Self {
        Self {
            agent_discovery: Arc::new(DefaultWorkspaceAgentDiscovery),
            registry_setup: Arc::new(DefaultWorkspaceToolRegistrySetup::default()),
            file_ops_tool_construction: Arc::new(DefaultWorkspaceFileOpsToolConstruction),
            subagent_attachment: Arc::new(DefaultAcpSubagentControllerAttacher),
            mcp_attachment: Arc::new(DefaultAcpMcpClientAttacher),
            tool_projection: Arc::new(DefaultAcpWorkspaceToolProjection),
            permission_construction: Arc::new(DefaultAcpPermissionPrompterConstruction),
        }
    }
}

struct DefaultWorkspaceAgentDiscovery;

impl WorkspaceAgentDiscovery for DefaultWorkspaceAgentDiscovery {
    fn discover(
        &self,
        input: &SubagentDiscoveryInput,
        default_primary_agent: &str,
    ) -> anyhow::Result<PrimaryAgentCatalogue> {
        let discovered = discover_subagents(input)?;
        Ok(PrimaryAgentCatalogue::from_specs_with_default(&discovered.effective, default_primary_agent))
    }
}

/// Default registry setup that optionally installs an agent-wide collector.
#[derive(Default)]
struct DefaultWorkspaceToolRegistrySetup {
    metrics: Option<Arc<MetricsCollector>>,
}

impl WorkspaceToolRegistrySetup for DefaultWorkspaceToolRegistrySetup {
    fn initialize<'a>(
        &'a self,
        workspace_root: &'a Path,
        runtime_config: &'a SessionWorkspaceRuntimeConfig,
        vt_config: Option<&'a DiapasonConfig>,
    ) -> BoxFuture<'a, anyhow::Result<CoreToolRegistry>> {
        Box::pin(async move {
            let mut local_tool_registry = CoreToolRegistry::new(workspace_root.to_path_buf()).await;
            if let Some(metrics) = &self.metrics {
                local_tool_registry.set_metrics_collector(Arc::clone(metrics));
            }
            configure_acp_tool_call_limits(&local_tool_registry, vt_config);
            local_tool_registry
                .apply_tool_runtime_config(&runtime_config.commands_config, &runtime_config.tools_config)
                .await
                .context("applying tool runtime configuration to ACP workspace registry")?;
            Ok(local_tool_registry)
        })
    }
}

struct DefaultWorkspaceFileOpsToolConstruction;

impl WorkspaceFileOpsToolConstruction for DefaultWorkspaceFileOpsToolConstruction {
    fn construct(
        &self,
        workspace_root: &Path,
        zed_config: &AgentClientProtocolZedConfig,
    ) -> anyhow::Result<Option<FileOpsTool>> {
        Ok(create_file_ops_tool(workspace_root, zed_config))
    }
}

struct DefaultAcpWorkspaceToolProjection;

impl AcpWorkspaceToolProjection for DefaultAcpWorkspaceToolProjection {
    fn project<'a>(
        &'a self,
        local_tool_registry: &'a CoreToolRegistry,
        workspace_root: &'a Path,
        zed_config: &'a AgentClientProtocolZedConfig,
        tools_config: &'a ToolsConfig,
        list_files_enabled: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<AcpToolRegistry>>> {
        Box::pin(async move {
            let local_definitions = local_tool_registry
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
            Ok(Arc::new(AcpToolRegistry::new(
                workspace_root,
                zed_config.tools.read_file,
                list_files_enabled,
                local_definitions,
            )))
        })
    }
}

struct DefaultAcpPermissionPrompterConstruction;

impl AcpPermissionPrompterConstruction for DefaultAcpPermissionPrompterConstruction {
    fn construct(
        &self,
        registry: Arc<AcpToolRegistry>,
        skip_confirmations: bool,
    ) -> anyhow::Result<Arc<dyn AcpPermissionPrompter + Send + Sync>> {
        Ok(Arc::new(DefaultPermissionPrompter::with_skip_confirmations(
            Arc::clone(&registry) as Arc<_>,
            skip_confirmations,
        )))
    }
}

pub(super) async fn build_launch_workspace_runtime(
    setup: &ZedAgentSetup<'_>,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<SessionWorkspaceRuntime> {
    let runtime_config = SessionWorkspaceRuntimeConfig {
        zed_config: setup.zed_config.clone(),
        tools_config: setup.tools_config.clone(),
        commands_config: setup.commands_config.clone(),
        custom_providers: setup.custom_providers.to_vec(),
        skip_confirmations: setup.skip_confirmations,
    };
    let prepared = prepare_launch_workspace(setup, &runtime_config, ports)?;
    build_prepared_workspace_runtime(prepared, false, ports).await
}

impl SessionWorkspaceRuntime {
    async fn build(setup: SessionWorkspaceRuntimeSetup<'_>, ports: &WorkspaceSetupPorts) -> anyhow::Result<Self> {
        let prepared = prepare_session_workspace(setup, ports).await?;
        build_prepared_workspace_runtime(prepared, true, ports).await
    }

    fn report_optional_setup_outcomes(&self) {
        report_optional_workspace_setup_outcomes(&self.optional_setup_outcomes);
    }
}

fn build_session_workspace_runtime<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
    ports: &'a WorkspaceSetupPorts,
) -> BoxFuture<'a, anyhow::Result<Arc<SessionWorkspaceRuntime>>> {
    Box::pin(async move { SessionWorkspaceRuntime::build(setup, ports).await.map(Arc::new) })
}

impl ZedAgent {
    /// Returns the workspace runtime leased to this session.
    ///
    /// `defer_background_restore` marks an acquisition made by a resumed attach.
    /// Such an attach restores no background work before its required history
    /// checkpoint, because a restored child can outlive an attach that then
    /// fails: the fail-closed path releases the workspace lease, but shutdown
    /// aborts only tracked children, never restored background children. A
    /// runtime it builds therefore skips the workspace controller's restore of
    /// ownerless records and records the debt instead.
    ///
    /// A resumed attach never releases or runs the debt here. It releases the
    /// debt itself, after its required checkpoint, once its owner migration is
    /// reconciled; an acquisition cannot infer that reconciliation from its own
    /// success, because a successful migration says nothing about the rows an
    /// earlier resume left ownerless.
    ///
    /// A session that is not a resumed attach restores at build time and may
    /// complete a restore a resumed attach has already released. It cannot
    /// release a blocked one: only a resume can show that the rows it launched
    /// carry their owner, and releasing on anything less could relaunch work a
    /// live session still owns.
    ///
    /// Runtimes are cached by workspace root alone, so a later attach that
    /// reuses one keeps the policy of the build that created it. A resume can
    /// therefore reuse a runtime built with `defer_background_restore = false`.
    /// That does not risk a duplicate process: the resumed attach reconciles
    /// the workspace controller with its owner migration, adopting the rows it
    /// still holds ownerless before the session controller is built.
    ///
    /// The launch runtime is returned as-is for its own workspace root, so it
    /// has already made that choice; `defer_background_restore` cannot undo a
    /// restore the launch controller ran before the session attached. A resume
    /// therefore reconciles that controller with its owner migration instead of
    /// relying on the deferral for the launch root.
    pub(super) async fn workspace_runtime_for(
        &self,
        workspace: &AcpSessionWorkspace,
        defer_background_restore: bool,
    ) -> anyhow::Result<SessionWorkspaceLease> {
        let workspace_root = workspace.path().to_path_buf();
        if self.launch_workspace_runtime.workspace_root == workspace_root {
            return Ok(SessionWorkspaceLease::unmanaged(Arc::clone(&self.launch_workspace_runtime)));
        }
        loop {
            let lookup = {
                let mut runtimes = self.workspace_runtimes.lock().await;
                runtimes.lookup_or_reserve_initialization(&workspace_root)
            };
            let (runtime, runtime_cell) = match lookup {
                WorkspaceRuntimeLookup::Ready(runtime) => (runtime, None),
                WorkspaceRuntimeLookup::Initialize(runtime_cell) => {
                    let runtime_result = runtime_cell
                        .get_or_try_init(|| {
                            build_session_workspace_runtime(
                                SessionWorkspaceRuntimeSetup {
                                    base_config: &self.config,
                                    workspace,
                                    runtime_config: &self.workspace_runtime_config,
                                    vt_config: self.vt_config.as_deref(),
                                    defer_background_restore,
                                },
                                &self.workspace_setup_ports,
                            )
                        })
                        .await;
                    match runtime_result {
                        Ok(runtime) => {
                            let runtime = Arc::clone(runtime);
                            self.workspace_runtimes.lock().await.store_initialized_runtime(
                                workspace_root.clone(),
                                &runtime,
                                &runtime_cell,
                            );
                            (runtime, Some(runtime_cell))
                        }
                        Err(error) => {
                            self.workspace_runtimes
                                .lock()
                                .await
                                .remove_in_flight_if_same(&workspace_root, &runtime_cell);
                            return Err(error);
                        }
                    }
                }
            };

            if let Some(lease) = SessionWorkspaceLease::acquire(Arc::clone(&runtime)).await? {
                // A resumed attach defers everything to its own release after
                // its required checkpoint, so this acquisition neither releases
                // nor runs the debt. Any other acquisition may complete a
                // restore a resume already released; claiming a still-blocked
                // debt is impossible, because only the release makes it payable
                // and no new session can establish that reconciliation.
                if !defer_background_restore {
                    self.run_owed_workspace_background_restore(&runtime).await;
                }
                return Ok(lease);
            }
            self.workspace_runtimes
                .lock()
                .await
                .remove_runtime_if_same(&workspace_root, &runtime);
            drop(runtime_cell);
        }
    }

    /// Runs the workspace-scope restore a deferred build still owes, if any.
    ///
    /// The pass is claimed under one lock, so at most one acquisition of a
    /// runtime performs it at a time. The claim is discharged only by a restore
    /// that ran: a failed pass, or a caller cancelled while it runs, reinstates
    /// the debt so a later acquisition retries it instead of losing it.
    ///
    /// The debt is only payable once a resumed attach has established that the
    /// rows it launched carry their owner, and that attach calls this only
    /// after its required history checkpoint. The ownerless rows the restore
    /// takes are therefore genuinely nobody's, and a restored child can never
    /// outlive an attach that then fails.
    ///
    /// A runtime without a controller, or whose configuration disables the
    /// restore, leaves the debt untouched rather than discharging it: those
    /// settings are fixed for the runtime's lifetime, so every later
    /// acquisition takes the same early return without doing any work.
    pub(in crate::zed::agent) async fn run_owed_workspace_background_restore(&self, runtime: &SessionWorkspaceRuntime) {
        let Some(controller) = runtime.local_tool_registry.subagent_controller() else {
            return;
        };
        let Some(config) = self.vt_config.as_deref().filter(|config| config.subagents.enabled) else {
            return;
        };
        if !config.subagents.background.auto_restore {
            return;
        }
        let Some(claim) = runtime.claim_owed_background_restore() else {
            return;
        };
        let outcome = crate::zed::agent::optional_setup::restore_workspace_background_subagents(
            &controller,
            config,
            &runtime.workspace_root,
        )
        .await;
        let restored = matches!(&outcome, OptionalSetupOutcome::Attached);
        crate::zed::agent::optional_setup::report_optional_setup_outcome("background subagent restore", &outcome);
        if restored {
            claim.confirm();
        }
    }
}

fn prepare_launch_workspace<'a>(
    setup: &ZedAgentSetup<'a>,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<PreparedWorkspace<'a>> {
    let workspace_root = setup.config.workspace.clone();
    let workspace_hooks_gated = setup.primary_agents.workspace_hooks_gated(setup.vt_config);
    let file_ops_tool = construct_workspace_file_ops_tool(
        ports.file_ops_tool_construction.as_ref(),
        &workspace_root,
        &runtime_config.zed_config,
    )?;

    Ok(PreparedWorkspace {
        workspace_root,
        session_config: setup.config.clone(),
        runtime_config,
        vt_config: setup.vt_config,
        system_prompt: setup.system_prompt.clone(),
        primary_agents: setup.primary_agents.clone(),
        workspace_hooks_gated,
        file_ops_tool,
        // The launch runtime restores ownerless rows as the process starts; it
        // is not the resumed-session scope the deferral protects. It is built
        // before any session resumes, so it can restore, or hold in memory, a
        // row a resumed session later migrates to itself. A resume therefore
        // reconciles this controller with the migration before it restores its
        // own rows, and the ledger merge refuses to write an ownerless copy
        // over the owned record of the same launch.
        defer_background_restore: false,
    })
}

async fn prepare_session_workspace<'a>(
    setup: SessionWorkspaceRuntimeSetup<'a>,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<PreparedWorkspace<'a>> {
    let SessionWorkspaceRuntimeSetup {
        base_config,
        workspace,
        runtime_config,
        vt_config,
        defer_background_restore,
    } = setup;
    let workspace_root = workspace.path().to_path_buf();
    let mut session_config = base_config.clone();
    session_config.workspace = workspace_root.clone();
    let content = generate_system_instruction_with_config(&Default::default(), &workspace_root, vt_config).await;
    let system_prompt = content
        .parts
        .first()
        .and_then(|part| part.as_text())
        .map_or_else(String::new, ToString::to_string);
    let default_primary_agent = vt_config.map_or("duck", |config| config.default_primary_agent.as_str());
    let discovery_input = SubagentDiscoveryInput::new(workspace_root.clone());
    let primary_agents =
        discover_session_primary_agents_with(ports.agent_discovery.as_ref(), &discovery_input, default_primary_agent)?;
    let workspace_hooks_gated = primary_agents.workspace_hooks_gated(vt_config);
    let file_ops_tool = construct_workspace_file_ops_tool(
        ports.file_ops_tool_construction.as_ref(),
        &workspace_root,
        &runtime_config.zed_config,
    )?;

    Ok(PreparedWorkspace {
        workspace_root,
        session_config,
        runtime_config,
        vt_config,
        system_prompt,
        primary_agents,
        workspace_hooks_gated,
        file_ops_tool,
        defer_background_restore,
    })
}

/// Discovers primary agents for a session using the default workspace
/// discovery backend. Available only in tests.
#[cfg(test)]
fn discover_session_primary_agents(
    input: &SubagentDiscoveryInput,
    default_primary_agent: &str,
) -> anyhow::Result<PrimaryAgentCatalogue> {
    discover_session_primary_agents_with(&DefaultWorkspaceAgentDiscovery, input, default_primary_agent)
}

/// Discovers primary agents for a session through the supplied discovery port,
/// adding context to discovery errors.
fn discover_session_primary_agents_with(
    agent_discovery: &dyn WorkspaceAgentDiscovery,
    input: &SubagentDiscoveryInput,
    default_primary_agent: &str,
) -> anyhow::Result<PrimaryAgentCatalogue> {
    agent_discovery
        .discover(input, default_primary_agent)
        .context("discovering ACP workspace agents")
}

async fn initialize_workspace_tool_registry(
    prepared: &PreparedWorkspace<'_>,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<InitializedWorkspaceToolRegistry> {
    let local_tool_registry = ports
        .registry_setup
        .initialize(&prepared.workspace_root, prepared.runtime_config, prepared.vt_config)
        .await
        .context("setting up ACP workspace tool registry")?;
    let optional_setup_outcomes = attach_optional_workspace_components(
        OptionalWorkspaceComponentsSetup {
            registry: &local_tool_registry,
            agent_config: &prepared.session_config,
            custom_providers: &prepared.runtime_config.custom_providers,
            vt_config: prepared.vt_config,
            workspace_gated: prepared.workspace_hooks_gated,
            workspace_root: &prepared.workspace_root,
            defer_background_restore: prepared.defer_background_restore,
        },
        ports,
    )
    .await;
    Ok(InitializedWorkspaceToolRegistry {
        registry: local_tool_registry,
        optional_setup_outcomes,
    })
}

async fn build_prepared_workspace_runtime(
    prepared: PreparedWorkspace<'_>,
    shutdown_managed: bool,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<SessionWorkspaceRuntime> {
    let initialized_registry = initialize_workspace_tool_registry(&prepared, ports).await?;
    let tool_surface = build_acp_tool_surface(
        &initialized_registry.registry,
        &prepared.workspace_root,
        &prepared.runtime_config.zed_config,
        &prepared.runtime_config.tools_config,
        prepared.runtime_config.skip_confirmations,
        prepared.file_ops_tool.is_some(),
        ports,
    )
    .await?;

    // A deferred restore is not dropped, only postponed: the runtime records
    // that it still owes one, so a later acquisition of the same runtime can
    // run it once ownership is safe to reconcile. Every other build restored
    // at once and owes nothing.
    let pending_background_restore = if prepared.defer_background_restore {
        PendingWorkspaceRestore::BlockedByOwnerMigration
    } else {
        PendingWorkspaceRestore::NotOwed
    };
    let runtime = SessionWorkspaceRuntime {
        workspace_root: prepared.workspace_root,
        workspace_hooks_gated: prepared.workspace_hooks_gated,
        system_prompt: prepared.system_prompt,
        primary_agents: prepared.primary_agents,
        acp_tool_registry: tool_surface.registry,
        permission_prompter: tool_surface.permission_prompter,
        local_tool_registry: initialized_registry.registry,
        file_ops_tool: prepared.file_ops_tool,
        optional_setup_outcomes: initialized_registry.optional_setup_outcomes,
        shutdown_managed,
        lifecycle: Mutex::new(WorkspaceRuntimeLifecycle {
            state: WorkspaceRuntimeLifecycleState::Open,
            active_sessions: 0,
        }),
        lifecycle_changed: Notify::new(),
        pending_background_restore: Mutex::new(pending_background_restore),
    };
    runtime.report_optional_setup_outcomes();
    Ok(runtime)
}

async fn attach_optional_workspace_components(
    setup: OptionalWorkspaceComponentsSetup<'_>,
    ports: &WorkspaceSetupPorts,
) -> OptionalWorkspaceSetupOutcomes {
    let subagent_outcome = ports
        .subagent_attachment
        .attach(AcpSubagentControllerSetup {
            owner_session_id: None,
            registry: setup.registry,
            agent_config: setup.agent_config,
            custom_providers: setup.custom_providers,
            vt_config: setup.vt_config,
            workspace_gated: setup.workspace_gated,
            defer_background_restore: setup.defer_background_restore,
            // The workspace controller has no owner, so it has no owner
            // migration to retry.
            owner_migration_failed: false,
        })
        .await;
    let mcp_outcome = ports
        .mcp_attachment
        .attach(setup.registry, setup.vt_config, setup.workspace_root)
        .await;
    preserve_optional_workspace_setup_outcomes(subagent_outcome, mcp_outcome)
}

fn preserve_optional_workspace_setup_outcomes(
    subagent_controller: OptionalSetupOutcome,
    mcp_client: OptionalSetupOutcome,
) -> OptionalWorkspaceSetupOutcomes {
    OptionalWorkspaceSetupOutcomes { subagent_controller, mcp_client }
}

fn create_file_ops_tool(workspace_root: &Path, zed_config: &AgentClientProtocolZedConfig) -> Option<FileOpsTool> {
    if zed_config.tools.list_files {
        let search_root = workspace_root.to_path_buf();
        Some(FileOpsTool::new(workspace_root.to_path_buf(), Arc::new(GrepSearchManager::new(search_root))))
    } else {
        None
    }
}

fn construct_workspace_file_ops_tool(
    construction: &dyn WorkspaceFileOpsToolConstruction,
    workspace_root: &Path,
    zed_config: &AgentClientProtocolZedConfig,
) -> anyhow::Result<Option<FileOpsTool>> {
    construction
        .construct(workspace_root, zed_config)
        .context("constructing ACP workspace file operations tool")
}

async fn build_acp_tool_surface(
    local_tool_registry: &CoreToolRegistry,
    workspace_root: &Path,
    zed_config: &AgentClientProtocolZedConfig,
    tools_config: &ToolsConfig,
    skip_confirmations: bool,
    list_files_enabled: bool,
    ports: &WorkspaceSetupPorts,
) -> anyhow::Result<AcpWorkspaceToolSurface> {
    let registry = ports
        .tool_projection
        .project(local_tool_registry, workspace_root, zed_config, tools_config, list_files_enabled)
        .await
        .context("projecting ACP workspace tool surface")?;
    let permission_prompter = ports
        .permission_construction
        .construct(Arc::clone(&registry), skip_confirmations)
        .context("constructing ACP workspace permission prompter")?;

    Ok(AcpWorkspaceToolSurface { registry, permission_prompter })
}

#[cfg(test)]
mod setup_tests;

#[cfg(test)]
mod tests;
