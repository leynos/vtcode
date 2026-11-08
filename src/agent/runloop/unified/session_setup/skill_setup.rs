use crate::agent::runloop::ResumeSession;
use crate::agent::runloop::unified::session_setup::init::refresh_tool_snapshot;
use crate::agent::runloop::unified::tool_catalogue::{ToolCatalogueState, tool_catalogue_change_notifier};
use anyhow::{Context, Result};
use diapason_config::constants::tools as tool_constants;
use diapason_core::config::loader::DiapasonConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::llm::provider as uni;
use diapason_core::tools::ToolRegistry;
use diapason_core::tools::handlers::{DeferredToolPolicy, ToolModelCapabilities};
use diapason_core::tools::native_cgp_tool_factory;
use diapason_core::tools::registry::ToolCatalogueSource;
use hashbrown::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub(crate) struct SkillSetupState {
    pub active_skills_map: Arc<RwLock<HashMap<String, diapason_core::skills::types::Skill>>>,
}

pub(crate) async fn discover_skills(config: &CoreAgentConfig, resume: Option<&ResumeSession>) -> SkillSetupState {
    let active_skills_map = Arc::new(RwLock::new(HashMap::new()));

    info!(
        workspace = %config.workspace.display(),
        "Deferring skill discovery until an explicit /skills command"
    );

    if let Some(resume_session) = resume
        && !resume_session.loaded_skills().is_empty()
    {
        warn!("Skipping loaded skill restore during startup; use /skills load to reactivate session skills");
    }

    SkillSetupState { active_skills_map }
}

/// Register the skill tools and refresh the model tool snapshot. The skill
/// runtime shares the registry, tool catalogue, and fork executor with the
/// session.
pub(crate) async fn register_skill_tools(
    tool_registry: &mut ToolRegistry,
    tools: &Arc<RwLock<Vec<uni::ToolDefinition>>>,
    tool_catalogue: &Arc<ToolCatalogueState>,
    config: &CoreAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    tool_documentation_mode: diapason_core::config::ToolDocumentationMode,
    deferred_tool_policy: DeferredToolPolicy,
    anthropic_native_memory_enabled: bool,
    skill_setup: &SkillSetupState,
) -> Result<()> {
    let runtime = diapason_core::tools::skills::SkillToolSessionRuntime::new(
        Arc::new(tool_registry.clone()),
        Some(Arc::clone(tools)),
        tool_documentation_mode,
        ToolModelCapabilities::for_model_name(&config.model),
        Some(tool_catalogue_change_notifier(tool_catalogue)),
    )
    .with_tool_profile(vt_cfg.map(|cfg| cfg.tools.profile).unwrap_or_default())
    .with_fork_executor(Arc::new(diapason_core::skills::executor::ChildAgentSkillExecutor::new(
        Arc::new(tool_registry.clone()),
        diapason_core::skills::executor::ForkSkillRuntimeConfig {
            workspace: config.workspace.clone(),
            model: config.model.clone(),
            api_key: config.api_key.clone(),
            openai_chatgpt_auth: config.openai_chatgpt_auth.clone(),
            vt_cfg: vt_cfg.cloned(),
        },
    )))
    .with_deferred_tool_policy(deferred_tool_policy.clone());
    let runtime = runtime.with_anthropic_native_memory_enabled(anthropic_native_memory_enabled);

    register_list_skills_tool(tool_registry, config.workspace.clone(), Arc::clone(&skill_setup.active_skills_map))
        .await?;
    register_load_skill_resource_tool(tool_registry, Arc::clone(&skill_setup.active_skills_map)).await?;
    register_load_skill_tool(
        tool_registry,
        config.workspace.clone(),
        Arc::clone(&skill_setup.active_skills_map),
        runtime,
    )
    .await?;

    refresh_tool_snapshot(
        tool_registry,
        tools,
        tool_catalogue,
        config,
        vt_cfg,
        tool_documentation_mode,
        &deferred_tool_policy,
    )
    .await;
    Ok(())
}

/// Register the `list_skills` tool, wiring both the built-in registration and
/// its native per-call factory.
async fn register_list_skills_tool(
    tool_registry: &mut ToolRegistry,
    workspace_root: std::path::PathBuf,
    active_skills_map: Arc<RwLock<HashMap<String, diapason_core::skills::types::Skill>>>,
) -> Result<()> {
    let list_skills_tool =
        diapason_core::tools::skills::ListSkillsTool::new(workspace_root.clone(), Arc::clone(&active_skills_map));
    let list_skills_reg = diapason_core::tools::registry::ToolRegistration::from_tool_instance(
        tool_constants::LIST_SKILLS,
        diapason_core::config::types::CapabilityLevel::Basic,
        list_skills_tool,
    )
    .with_catalogue_source(ToolCatalogueSource::Builtin)
    .with_native_cgp_factory(native_cgp_tool_factory({
        let workspace_root = workspace_root.clone();
        let active_skills_map = Arc::clone(&active_skills_map);
        move || {
            diapason_core::tools::skills::ListSkillsTool::new(workspace_root.clone(), Arc::clone(&active_skills_map))
        }
    }));
    tool_registry
        .register_tool(list_skills_reg)
        .await
        .context("Failed to register list_skills tool")?;
    Ok(())
}

/// Register the `load_skill_resource` tool against the shared active-skills map.
async fn register_load_skill_resource_tool(
    tool_registry: &mut ToolRegistry,
    active_skills_map: Arc<RwLock<HashMap<String, diapason_core::skills::types::Skill>>>,
) -> Result<()> {
    let load_resource_tool = diapason_core::tools::skills::LoadSkillResourceTool::new(Arc::clone(&active_skills_map));
    let load_resource_reg = diapason_core::tools::registry::ToolRegistration::from_tool_instance(
        tool_constants::LOAD_SKILL_RESOURCE,
        diapason_core::config::types::CapabilityLevel::Basic,
        load_resource_tool,
    )
    .with_catalogue_source(ToolCatalogueSource::Builtin)
    .with_native_cgp_factory(native_cgp_tool_factory({
        let active_skills_map = Arc::clone(&active_skills_map);
        move || diapason_core::tools::skills::LoadSkillResourceTool::new(Arc::clone(&active_skills_map))
    }));
    tool_registry
        .register_tool(load_resource_reg)
        .await
        .context("Failed to register load_skill_resource tool")?;
    Ok(())
}

/// Register the `load_skill` tool, passing the skill session runtime into both
/// the built-in registration and its native factory.
async fn register_load_skill_tool(
    tool_registry: &mut ToolRegistry,
    workspace_root: std::path::PathBuf,
    active_skills_map: Arc<RwLock<HashMap<String, diapason_core::skills::types::Skill>>>,
    runtime: diapason_core::tools::skills::SkillToolSessionRuntime,
) -> Result<()> {
    let load_skill_tool = diapason_core::tools::skills::LoadSkillTool::new(
        workspace_root.clone(),
        Arc::clone(&active_skills_map),
        runtime.clone(),
    );
    let load_skill_reg = diapason_core::tools::registry::ToolRegistration::from_tool_instance(
        tool_constants::LOAD_SKILL,
        diapason_core::config::types::CapabilityLevel::Basic,
        load_skill_tool,
    )
    .with_catalogue_source(ToolCatalogueSource::Builtin)
    .with_native_cgp_factory(native_cgp_tool_factory({
        let workspace_root = workspace_root.clone();
        let active_skills_map = Arc::clone(&active_skills_map);
        let runtime = runtime.clone();
        move || {
            diapason_core::tools::skills::LoadSkillTool::new(
                workspace_root.clone(),
                Arc::clone(&active_skills_map),
                runtime.clone(),
            )
        }
    }));
    tool_registry
        .register_tool(load_skill_reg)
        .await
        .context("Failed to register load_skill tool")?;
    Ok(())
}
