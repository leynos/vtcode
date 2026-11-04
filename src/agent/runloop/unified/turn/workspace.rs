use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use diapason_core::SimpleIndexer;
use diapason_core::config::loader::{ConfigManager, DiapasonConfig};
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use tokio::task;

use crate::agent::agents::apply_live_reload_overrides;

async fn blocking_task<F, R>(label: &str, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: Send + 'static,
{
    task::spawn_blocking(f)
        .await
        .map_err(|err| anyhow!("failed to join {label} task: {err}"))?
}

pub(crate) async fn bootstrap_config_files(workspace: PathBuf, force: bool) -> Result<Vec<String>> {
    blocking_task("configuration bootstrap", move || DiapasonConfig::bootstrap_project(&workspace, force)).await
}

pub(crate) async fn build_workspace_index(workspace: PathBuf) -> Result<()> {
    blocking_task("workspace indexing", move || -> Result<()> {
        let mut indexer = SimpleIndexer::new(workspace.clone());
        indexer.init()?;
        indexer.index_directory(&workspace)?;
        Ok(())
    })
    .await
}

async fn load_workspace_config_snapshot(workspace: &Path) -> Result<DiapasonConfig> {
    let workspace_buf = workspace.to_path_buf();
    blocking_task("workspace config load", move || {
        ConfigManager::invalidate_workspace_cache(&workspace_buf);
        ConfigManager::load_from_workspace_with_repository_repair(&workspace_buf)
            .map(|manager| manager.config().clone())
    })
    .await
}

pub(crate) async fn refresh_vt_config(
    workspace: &Path,
    runtime_cfg: &CoreAgentConfig,
    vt_cfg: &mut Option<DiapasonConfig>,
) -> Result<()> {
    let snapshot = load_workspace_config_snapshot(workspace).await?;
    apply_workspace_config_snapshot(snapshot, runtime_cfg, vt_cfg)
}

pub(crate) fn apply_workspace_config_snapshot(
    mut snapshot: DiapasonConfig,
    runtime_cfg: &CoreAgentConfig,
    vt_cfg: &mut Option<DiapasonConfig>,
) -> Result<()> {
    apply_live_reload_overrides(Some(&mut snapshot), runtime_cfg);
    // Skip the remove+register cycle when custom providers haven't changed.
    // This function is called on every turn and on live-reload; re-registering
    // unchanged providers wastes factory lock + closure rebuild work and
    // produces repeated "Registered custom provider" log noise.
    let custom_providers_unchanged = vt_cfg
        .as_ref()
        .is_some_and(|old| old.custom_providers == snapshot.custom_providers);
    if !custom_providers_unchanged {
        diapason_core::llm::factory::register_custom_providers(&snapshot.custom_providers);
    }
    *vt_cfg = Some(snapshot);
    Ok(())
}

pub(crate) fn apply_workspace_config_to_registry(
    tool_registry: &diapason_core::tools::registry::ToolRegistry,
    vt_cfg: &DiapasonConfig,
) -> Result<()> {
    tool_registry.apply_commands_config(&vt_cfg.commands);
    tool_registry.apply_permissions_config(&vt_cfg.permissions);
    tool_registry.apply_sandbox_config(&vt_cfg.sandbox);
    tool_registry.apply_timeout_policy(&vt_cfg.timeouts);
    Ok(())
}
