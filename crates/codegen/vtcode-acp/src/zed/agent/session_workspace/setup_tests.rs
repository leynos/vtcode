//! Tests the injected boundaries used to assemble ACP workspace runtimes.
use super::*;
use assert_fs::TempDir;
use futures::future::BoxFuture;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vtcode_config::{SubagentDiscoveryInput, VTCodeConfig};
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};

struct FailingAgentDiscovery;

impl WorkspaceAgentDiscovery for FailingAgentDiscovery {
    fn discover(
        &self,
        _input: &SubagentDiscoveryInput,
        _default_primary_agent: &str,
    ) -> anyhow::Result<PrimaryAgentCatalog> {
        anyhow::bail!("agent discovery failure")
    }
}

struct FailingRegistrySetup;

impl WorkspaceToolRegistrySetup for FailingRegistrySetup {
    fn initialize<'a>(
        &'a self,
        _workspace_root: &'a Path,
        _runtime_config: &'a SessionWorkspaceRuntimeConfig,
        _vt_config: Option<&'a VTCodeConfig>,
    ) -> BoxFuture<'a, anyhow::Result<CoreToolRegistry>> {
        Box::pin(async { anyhow::bail!("registry setup failure") })
    }
}

struct FailingFileOpsToolConstruction;

impl WorkspaceFileOpsToolConstruction for FailingFileOpsToolConstruction {
    fn construct(
        &self,
        _workspace_root: &Path,
        _zed_config: &AgentClientProtocolZedConfig,
    ) -> anyhow::Result<Option<FileOpsTool>> {
        anyhow::bail!("file operations construction failure")
    }
}

struct DegradedSubagentAttachment;

impl AcpSubagentControllerAttacher for DegradedSubagentAttachment {
    fn attach<'a>(&'a self, _setup: AcpSubagentControllerSetup<'a>) -> BoxFuture<'a, OptionalSetupOutcome> {
        Box::pin(async {
            OptionalSetupOutcome::AttachedWithDegradation(vec!["background restore unavailable".to_string()])
        })
    }
}

struct DisabledMcpAttachment;

impl AcpMcpClientAttacher for DisabledMcpAttachment {
    fn attach<'a>(
        &'a self,
        _registry: &'a CoreToolRegistry,
        _vt_config: Option<&'a VTCodeConfig>,
        _workspace_root: &'a Path,
    ) -> BoxFuture<'a, OptionalSetupOutcome> {
        Box::pin(async { OptionalSetupOutcome::DisabledByConfiguration })
    }
}

struct FailingToolProjection;

impl AcpWorkspaceToolProjection for FailingToolProjection {
    fn project<'a>(
        &'a self,
        _local_tool_registry: &'a CoreToolRegistry,
        _workspace_root: &'a Path,
        _zed_config: &'a AgentClientProtocolZedConfig,
        _tools_config: &'a ToolsConfig,
        _list_files_enabled: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<AcpToolRegistry>>> {
        Box::pin(async { anyhow::bail!("tool projection failure") })
    }
}

struct EmptyToolProjection;

impl AcpWorkspaceToolProjection for EmptyToolProjection {
    fn project<'a>(
        &'a self,
        _local_tool_registry: &'a CoreToolRegistry,
        workspace_root: &'a Path,
        zed_config: &'a AgentClientProtocolZedConfig,
        _tools_config: &'a ToolsConfig,
        list_files_enabled: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<AcpToolRegistry>>> {
        Box::pin(async move {
            Ok(Arc::new(AcpToolRegistry::new(
                workspace_root,
                zed_config.tools.read_file,
                list_files_enabled,
                Vec::new(),
            )))
        })
    }
}

struct FailingPermissionPrompterConstruction;

impl AcpPermissionPrompterConstruction for FailingPermissionPrompterConstruction {
    fn construct(
        &self,
        _registry: Arc<AcpToolRegistry>,
        _skip_confirmations: bool,
    ) -> anyhow::Result<Arc<dyn AcpPermissionPrompter + Send + Sync>> {
        anyhow::bail!("permission setup failure")
    }
}

fn runtime_config() -> SessionWorkspaceRuntimeConfig {
    SessionWorkspaceRuntimeConfig {
        zed_config: AgentClientProtocolZedConfig::default(),
        tools_config: ToolsConfig::default(),
        commands_config: CommandsConfig::default(),
        custom_providers: Vec::new(),
        skip_confirmations: false,
    }
}

fn core_agent_config(workspace: &Path) -> CoreAgentConfig {
    CoreAgentConfig {
        model: "test-model".to_string(),
        api_key: String::new(),
        provider: "test-provider".to_string(),
        api_key_env: "TEST_API_KEY".to_string(),
        workspace: workspace.to_path_buf(),
        verbose: false,
        quiet: false,
        theme: "test".to_string(),
        reasoning_effort: ReasoningEffortLevel::Low,
        ui_surface: UiSurfacePreference::default(),
        prompt_cache: PromptCachingConfig::default(),
        model_source: ModelSelectionSource::WorkspaceConfig,
        custom_api_keys: BTreeMap::new(),
        checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
        checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
        max_conversation_turns: 1000,
        model_behaviour: None,
        openai_chatgpt_auth: None,
    }
}

fn prepared_workspace<'a>(
    workspace: &Path,
    runtime_config: &'a SessionWorkspaceRuntimeConfig,
) -> PreparedWorkspace<'a> {
    PreparedWorkspace {
        workspace_root: workspace.to_path_buf(),
        session_config: core_agent_config(workspace),
        runtime_config,
        vt_config: None,
        system_prompt: "test prompt".to_string(),
        primary_agents: PrimaryAgentCatalog::from_specs_with_default(&[], "duck"),
        workspace_hooks_gated: false,
        file_ops_tool: None,
    }
}

#[test]
fn workspace_discovery_reports_an_injected_failure_at_its_boundary() {
    let mut input = SubagentDiscoveryInput::new(PathBuf::from("/workspace"));
    input.include_user_agents = false;

    let error = discover_session_primary_agents_with(&FailingAgentDiscovery, &input, "duck")
        .expect_err("injected discovery failure must be returned");

    assert!(format!("{error:#}").contains("discovering ACP workspace agents"));
    assert_eq!(error.root_cause().to_string(), "agent discovery failure");
}

#[test]
fn workspace_file_ops_construction_reports_an_injected_stage_failure() {
    let temp = TempDir::new().expect("temporary workspace");

    let error = match construct_workspace_file_ops_tool(
        &FailingFileOpsToolConstruction,
        temp.path(),
        &AgentClientProtocolZedConfig::default(),
    ) {
        Ok(_) => panic!("injected file operations construction failure must stop setup"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("constructing ACP workspace file operations tool"));
    assert_eq!(error.root_cause().to_string(), "file operations construction failure");
}

#[tokio::test]
async fn workspace_registry_setup_reports_an_injected_stage_failure() {
    let temp = TempDir::new().expect("temporary workspace");
    let runtime_config = runtime_config();
    let prepared = prepared_workspace(temp.path(), &runtime_config);
    let ports = WorkspaceSetupPorts {
        registry_setup: Arc::new(FailingRegistrySetup),
        ..WorkspaceSetupPorts::default()
    };

    let error = match initialize_workspace_tool_registry(&prepared, &ports).await {
        Ok(_) => panic!("injected registry failure must stop setup"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("setting up ACP workspace tool registry"));
    assert_eq!(error.root_cause().to_string(), "registry setup failure");
}

#[tokio::test]
async fn optional_workspace_attachments_preserve_degraded_and_disabled_outcomes() {
    let temp = TempDir::new().expect("temporary workspace");
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let agent_config = core_agent_config(temp.path());
    let ports = WorkspaceSetupPorts {
        subagent_attachment: Arc::new(DegradedSubagentAttachment),
        mcp_attachment: Arc::new(DisabledMcpAttachment),
        ..WorkspaceSetupPorts::default()
    };

    let outcomes = attach_optional_workspace_components(
        OptionalWorkspaceComponentsSetup {
            registry: &registry,
            agent_config: &agent_config,
            custom_providers: &[],
            vt_config: None,
            workspace_gated: false,
            workspace_root: temp.path(),
        },
        &ports,
    )
    .await;

    assert_eq!(
        outcomes,
        OptionalWorkspaceSetupOutcomes {
            subagent_controller: OptionalSetupOutcome::AttachedWithDegradation(vec![
                "background restore unavailable".to_string(),
            ]),
            mcp_client: OptionalSetupOutcome::DisabledByConfiguration,
        }
    );
}

#[tokio::test]
async fn acp_tool_projection_reports_an_injected_stage_failure() {
    let temp = TempDir::new().expect("temporary workspace");
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let ports = WorkspaceSetupPorts {
        tool_projection: Arc::new(FailingToolProjection),
        ..WorkspaceSetupPorts::default()
    };

    let error = match build_acp_tool_surface(
        &registry,
        temp.path(),
        &AgentClientProtocolZedConfig::default(),
        &ToolsConfig::default(),
        false,
        false,
        &ports,
    )
    .await
    {
        Ok(_) => panic!("injected tool projection failure must stop setup"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("projecting ACP workspace tool surface"));
    assert_eq!(error.root_cause().to_string(), "tool projection failure");
}

#[tokio::test]
async fn permission_construction_reports_an_injected_stage_failure() {
    let temp = TempDir::new().expect("temporary workspace");
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let ports = WorkspaceSetupPorts {
        tool_projection: Arc::new(EmptyToolProjection),
        permission_construction: Arc::new(FailingPermissionPrompterConstruction),
        ..WorkspaceSetupPorts::default()
    };

    let error = match build_acp_tool_surface(
        &registry,
        temp.path(),
        &AgentClientProtocolZedConfig::default(),
        &ToolsConfig::default(),
        false,
        false,
        &ports,
    )
    .await
    {
        Ok(_) => panic!("injected permission construction failure must stop setup"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("constructing ACP workspace permission prompter"));
    assert_eq!(error.root_cause().to_string(), "permission setup failure");
}
