use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use diapason_commons::DiapasonPaths;
use diapason_commons::canonicalize;
use diapason_commons::paths::normalize_path;
use diapason_config::defaults;
use diapason_config::loader::layers::{ConfigLayerMetadata, ConfigLayerSource};
use diapason_config::loader::{
    ConfigBuilder, ConfigManager, DiapasonConfig, explicit_config_path, fingerprint_str, merge_toml_values,
};
use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;

/// Request to read the effective configuration for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigReadRequest {
    /// Root directory of the workspace to read configuration for.
    pub workspace: PathBuf,
    /// Key-value overrides applied at runtime (e.g. CLI flags).
    #[serde(default)]
    pub runtime_overrides: Vec<(String, String)>,
}

/// View of a single configuration layer for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigLayerView {
    /// Origin of this layer (e.g. user, workspace, project).
    pub source: ConfigLayerSource,
    /// Metadata including name, version, and timestamps.
    pub metadata: ConfigLayerMetadata,
    /// Reason this layer was skipped during merging, if disabled.
    pub disabled_reason: Option<String>,
    /// Error message if this layer failed to load.
    pub error: Option<String>,
}

/// Response containing the merged effective configuration and layer details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigReadResponse {
    /// The fully merged configuration as a JSON value.
    pub effective_config: serde_json::Value,
    /// Fingerprint of the merged layer stack for change detection.
    pub merged_version: String,
    /// Ordered list of all configuration layers.
    pub layers: Vec<ConfigLayerView>,
    /// Map of config path to the layer that provides its effective value.
    pub origins: BTreeMap<String, ConfigLayerMetadata>,
}

/// The configuration layer target for a write operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigWriteTarget {
    /// User-level configuration in the canonical platform config directory.
    User,
    /// Workspace-level configuration (workspace root).
    Workspace,
    /// Project-level configuration (`.diapason/projects/<name>/config/`).
    Project,
}

impl ConfigWriteTarget {
    /// Stable lower-case name used in user-facing reset diagnostics.
    #[must_use]
    pub const fn layer_name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Workspace => "workspace",
            Self::Project => "project",
        }
    }
}

/// Strategy for applying a value to the configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigWriteStrategy {
    /// Replace the value at the given path unconditionally.
    Replace,
    /// Merge into existing tables; replace non-table values.
    Upsert,
}

/// Request to write a value to a specific configuration layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigWriteRequest {
    /// Root directory of the workspace.
    pub workspace: PathBuf,
    /// Which configuration layer to write to.
    pub target: ConfigWriteTarget,
    /// Dot-separated path to the configuration key (e.g. "agent.provider").
    pub path: String,
    /// The TOML value to write.
    pub value: TomlValue,
    /// How to apply the value to the existing configuration.
    pub strategy: ConfigWriteStrategy,
    /// Optional expected version of the target layer for optimistic concurrency.
    #[serde(default)]
    pub expected_layer_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverrideMetadata {
    pub source: ConfigLayerSource,
    pub metadata: ConfigLayerMetadata,
    pub effective_value: TomlValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigWriteResponse {
    pub merged_version: String,
    pub written_layer_version: String,
    pub effective_value: Option<TomlValue>,
    pub overridden_metadata: Option<OverrideMetadata>,
}

/// Request to clear one configuration layer.
///
/// Resetting a layer writes an empty TOML document to that layer's resolved
/// path. Lower-precedence layers remain active, and credential storage is not
/// touched. The optional expected version provides the same optimistic
/// concurrency guard as [`ConfigWriteRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigResetRequest {
    /// Root directory of the workspace whose effective configuration should be
    /// reloaded after the reset.
    pub workspace: PathBuf,
    /// Layer to clear. `User` is the target selected by the CLI's `--global`
    /// flag; `Project` is selected by `--project`.
    pub target: ConfigWriteTarget,
    /// Optional expected version of the selected layer.
    #[serde(default)]
    pub expected_layer_version: Option<String>,
    /// Optional exact path supplied by an already-open settings palette. The
    /// service validates it against the resolved layer path before clearing it.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// Result of clearing one configuration layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigResetResponse {
    /// Layer that was selected for reset.
    pub target: ConfigWriteTarget,
    /// Resolved configuration file that was cleared.
    pub path: PathBuf,
    /// Whether the target file existed before the reset.
    pub had_file: bool,
    /// Version of the selected layer before it was cleared, when loaded.
    pub previous_layer_version: Option<String>,
    /// Fingerprint of the effective layer stack after the reset.
    pub merged_version: String,
    /// Effective configuration after the reset.
    pub effective_config: serde_json::Value,
}

pub struct ConfigService;

impl ConfigService {
    /// Resolve the file used by a configuration layer for a workspace.
    ///
    /// This is shared by interactive settings and non-interactive commands so
    /// an explicit `--config` path, project profile, or canonical user path is
    /// never handled by a second path-resolution implementation.
    pub fn resolve_target_path(workspace: &Path, target: &ConfigWriteTarget) -> Result<PathBuf> {
        match ConfigManager::load_from_workspace_with_repository_repair(workspace) {
            Ok(manager) => resolve_target_path_from_manager(&manager, workspace, target),
            Err(error) => resolve_target_path_without_manager(workspace, target)
                .with_context(|| format!("Failed to resolve configuration target after load error: {error:#}")),
        }
    }

    pub fn read(request: ConfigReadRequest) -> Result<ConfigReadResponse> {
        let mut builder = ConfigBuilder::new().workspace(request.workspace.clone());
        if !request.runtime_overrides.is_empty() {
            builder = builder.cli_overrides(&request.runtime_overrides);
        }
        let manager = builder.build().context("Failed to build configuration")?;
        let (effective_toml, origins) = manager.layer_stack().effective_config_with_origins();
        let effective_config =
            serde_json::to_value(&effective_toml).context("Failed to serialize effective configuration to JSON")?;
        let merged_version = merged_version(manager.layer_stack().layers());

        let layers = manager
            .layer_stack()
            .layers()
            .iter()
            .map(|layer| ConfigLayerView {
                source: layer.source.clone(),
                metadata: layer.metadata.clone(),
                disabled_reason: layer.disabled_reason.as_ref().map(|reason| format!("{reason:?}")),
                error: layer.error.as_ref().map(|error| error.message.clone()),
            })
            .collect();

        let origins = origins.into_iter().collect::<BTreeMap<_, _>>();
        Ok(ConfigReadResponse { effective_config, merged_version, layers, origins })
    }

    pub fn write(request: ConfigWriteRequest) -> Result<ConfigWriteResponse> {
        if request.path.trim().is_empty() {
            bail!("Config path cannot be empty");
        }

        let manager = ConfigManager::load_from_workspace_with_repository_repair(&request.workspace)
            .with_context(|| format!("Failed to load workspace config from {}", request.workspace.display()))?;

        let target_path = resolve_target_path_from_manager(&manager, &request.workspace, &request.target)?;

        let current_version = manager
            .layer_stack()
            .layers()
            .iter()
            .find(|layer| source_matches_target(&layer.source, &request.target, &target_path))
            .map(|layer| layer.metadata.version.clone());

        if let Some(expected) = request.expected_layer_version.as_ref()
            && current_version.as_ref() != Some(expected)
        {
            bail!(
                "Layer version mismatch for {} (expected {}, got {})",
                target_path.display(),
                expected,
                current_version.unwrap_or_else(|| "<missing>".to_string())
            );
        }

        let mut target_toml = load_or_default_toml(&target_path)?;
        apply_write(&mut target_toml, &request.path, &request.value, request.strategy)?;

        let updated_config: DiapasonConfig = target_toml
            .clone()
            .try_into()
            .with_context(|| format!("Updated configuration at {} could not be deserialized", target_path.display()))?;
        updated_config.validate().context("Updated configuration failed validation")?;

        save_config_for_target(request.target, &target_path, &updated_config)
            .with_context(|| format!("Failed to write updated configuration to {}", target_path.display()))?;
        ConfigManager::invalidate_workspace_cache(&request.workspace);

        let reloaded_manager = ConfigManager::load_from_workspace_with_repository_repair(&request.workspace)
            .context("Failed to reload configuration after write")?;
        let (effective_toml, origins) = reloaded_manager.layer_stack().effective_config_with_origins();

        let written_layer = reloaded_manager
            .layer_stack()
            .layers()
            .iter()
            .find(|layer| source_matches_target(&layer.source, &request.target, &target_path))
            .with_context(|| format!("Unable to find written layer {} in reloaded stack", target_path.display()))?;

        let effective_value = get_value_by_path(&effective_toml, &request.path).cloned();
        let overridden_metadata = if let Some(origin) = origins.get(&request.path) {
            if origin.version != written_layer.metadata.version {
                let source = reloaded_manager
                    .layer_stack()
                    .layers()
                    .iter()
                    .find(|layer| layer.metadata.name == origin.name)
                    .map(|layer| layer.source.clone())
                    .unwrap_or(ConfigLayerSource::Runtime);

                effective_value.clone().map(|value| OverrideMetadata {
                    source,
                    metadata: origin.clone(),
                    effective_value: value,
                })
            } else {
                None
            }
        } else {
            None
        };

        Ok(ConfigWriteResponse {
            merged_version: merged_version(reloaded_manager.layer_stack().layers()),
            written_layer_version: written_layer.metadata.version.clone(),
            effective_value,
            overridden_metadata,
        })
    }

    /// Clear one configuration layer and reload the effective configuration.
    ///
    /// The selected file is replaced with an empty TOML document rather than
    /// deleting a directory or touching credential storage. Existing symlinks
    /// and non-regular files are rejected by the same private atomic writer
    /// used for normal configuration writes.
    pub fn reset(request: ConfigResetRequest) -> Result<ConfigResetResponse> {
        ConfigManager::invalidate_workspace_cache(&request.workspace);
        let manager_result = ConfigManager::load_from_workspace_with_repository_repair(&request.workspace);
        let (target, target_path) = match request.path.as_deref() {
            Some(requested_path) => {
                resolve_requested_reset_path(&request.workspace, manager_result.as_ref().ok(), requested_path)?
            }
            None => {
                let path = match &manager_result {
                    Ok(manager) => resolve_target_path_from_manager(manager, &request.workspace, &request.target)?,
                    Err(_) => resolve_target_path_without_manager(&request.workspace, &request.target)?,
                };
                (request.target, path)
            }
        };

        let previous_layer_version = manager_result.as_ref().ok().and_then(|manager| {
            manager
                .layer_stack()
                .layers()
                .iter()
                .find(|layer| source_matches_target(&layer.source, &target, &target_path))
                .map(|layer| layer.metadata.version.clone())
        });

        if let Some(expected) = request.expected_layer_version.as_ref()
            && previous_layer_version.as_ref() != Some(expected)
        {
            bail!(
                "Layer version mismatch for {} (expected {}, got {})",
                target_path.display(),
                expected,
                previous_layer_version.clone().unwrap_or_else(|| "<missing>".to_string())
            );
        }

        let had_file = match fs::symlink_metadata(&target_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("Refusing to reset symlinked config file: {}", target_path.display())
            }
            Ok(metadata) if !metadata.is_file() => {
                bail!("Config path is not a regular file: {}", target_path.display())
            }
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to inspect config: {}", target_path.display()));
            }
        };

        let is_explicit_session_path = explicit_config_path()
            .as_deref()
            .is_some_and(|explicit_path| same_config_path(explicit_path, &target_path));
        if had_file || is_explicit_session_path {
            DiapasonPaths::write_private_file_atomic(&target_path, b"")
                .with_context(|| format!("Failed to reset configuration file {}", target_path.display()))?;
        }

        ConfigManager::invalidate_workspace_cache(&request.workspace);
        let reloaded_manager = ConfigManager::load_from_workspace_with_repository_repair(&request.workspace)
            .context("Failed to reload configuration after reset")?;
        let (effective_toml, _) = reloaded_manager.layer_stack().effective_config_with_origins();
        let effective_config =
            serde_json::to_value(&effective_toml).context("Failed to serialize effective configuration after reset")?;

        Ok(ConfigResetResponse {
            target,
            path: target_path,
            had_file,
            previous_layer_version,
            merged_version: merged_version(reloaded_manager.layer_stack().layers()),
            effective_config,
        })
    }
}

fn merged_version(layers: &[diapason_config::loader::layers::ConfigLayerEntry]) -> String {
    let mut parts = Vec::with_capacity(layers.len());
    for layer in layers {
        if !layer.is_enabled() {
            continue;
        }
        parts.push(format!("{}:{}", layer.metadata.name, layer.metadata.version));
    }
    fingerprint_str(&parts.join("|"))
}

fn resolve_target_path_from_manager(
    manager: &ConfigManager,
    workspace: &Path,
    target: &ConfigWriteTarget,
) -> Result<PathBuf> {
    match target {
        ConfigWriteTarget::Workspace => {
            // Keep the configured spelling of the path instead of using the
            // canonicalized layer metadata. That preserves the final symlink
            // boundary for the no-follow writer to validate.
            if let Some(path) = explicit_config_path() {
                return Ok(path);
            }

            let provider = defaults::current_config_defaults();
            let workspace_root = manager.workspace_root().unwrap_or(workspace);
            let workspace_paths = provider.workspace_paths_for(workspace_root);
            let fallback = workspace_paths.config_dir().join(manager.config_file_name());
            let root = workspace_root.join(manager.config_file_name());
            if path_entry_exists(&root) || !path_entry_exists(&fallback) {
                Ok(root)
            } else {
                Ok(fallback)
            }
        }
        ConfigWriteTarget::User => {
            let provider = defaults::current_config_defaults();
            provider
                .canonical_user_config_path(manager.config_file_name())?
                .context("Could not resolve the canonical user configuration path")
        }
        ConfigWriteTarget::Project => {
            let provider = defaults::current_config_defaults();
            let workspace_root = manager.workspace_root().unwrap_or(workspace);
            let workspace_paths = provider.workspace_paths_for(workspace_root);
            let config_dir = workspace_paths.config_dir();
            let project_name = ConfigManager::current_project_name(workspace_root)
                .context("Could not resolve project name for project-level config")?;
            Ok(config_dir
                .join("projects")
                .join(project_name)
                .join("config")
                .join(manager.config_file_name()))
        }
    }
}

fn resolve_target_path_without_manager(workspace: &Path, target: &ConfigWriteTarget) -> Result<PathBuf> {
    let provider = defaults::current_config_defaults();
    let config_file_name = provider.config_file_name().to_string();

    match target {
        ConfigWriteTarget::Workspace => {
            if let Some(path) = explicit_config_path() {
                return Ok(path);
            }

            let workspace_paths = provider.workspace_paths_for(workspace);
            let fallback = workspace_paths.config_dir().join(&config_file_name);
            let root = workspace.join(&config_file_name);
            if path_entry_exists(&root) || !path_entry_exists(&fallback) {
                Ok(root)
            } else {
                Ok(fallback)
            }
        }
        ConfigWriteTarget::User => provider
            .canonical_user_config_path(&config_file_name)?
            .context("Could not resolve the canonical user configuration path"),
        ConfigWriteTarget::Project => {
            let workspace_paths = provider.workspace_paths_for(workspace);
            let project_name = ConfigManager::current_project_name(workspace)
                .context("Could not resolve project name for project-level config")?;
            Ok(workspace_paths
                .config_dir()
                .join("projects")
                .join(project_name)
                .join("config")
                .join(config_file_name))
        }
    }
}

fn resolve_requested_reset_path(
    workspace: &Path,
    manager: Option<&ConfigManager>,
    requested_path: &Path,
) -> Result<(ConfigWriteTarget, PathBuf)> {
    let mut candidates = Vec::new();
    let provider = defaults::current_config_defaults();
    let config_file_name = manager
        .map(|loaded| loaded.config_file_name().to_string())
        .unwrap_or_else(|| provider.config_file_name().to_string());

    if let Some(explicit_path) = explicit_config_path() {
        candidates.push((ConfigWriteTarget::Workspace, explicit_path));
    }

    let workspace_root = manager.and_then(ConfigManager::workspace_root).unwrap_or(workspace);
    let workspace_paths = provider.workspace_paths_for(workspace_root);
    let fallback_path = workspace_paths.config_dir().join(&config_file_name);
    let workspace_path = workspace_root.join(&config_file_name);
    candidates.push((ConfigWriteTarget::Workspace, fallback_path));
    candidates.push((ConfigWriteTarget::Workspace, workspace_path));

    if let Some(project_name) = ConfigManager::current_project_name(workspace_root) {
        candidates.push((
            ConfigWriteTarget::Project,
            workspace_paths
                .config_dir()
                .join("projects")
                .join(project_name)
                .join("config")
                .join(&config_file_name),
        ));
    }

    let mut user_paths = provider.home_config_paths(&config_file_name);
    if let Some(canonical_path) = provider.canonical_user_config_path(&config_file_name)?
        && !user_paths.iter().any(|path| same_config_path(path, &canonical_path))
    {
        user_paths.push(canonical_path);
    }
    if let Some(loaded) = manager {
        user_paths.extend(loaded.user_config_paths());
    }
    for user_path in user_paths {
        if !candidates.iter().any(|(_, existing)| same_config_path(existing, &user_path)) {
            candidates.push((ConfigWriteTarget::User, user_path));
        }
    }

    candidates
        .into_iter()
        .find(|(_, candidate)| same_config_path(requested_path, candidate))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Requested reset path {} is not a known writable Diapason configuration layer",
                requested_path.display()
            )
        })
}

fn source_matches_target(source: &ConfigLayerSource, target: &ConfigWriteTarget, path: &Path) -> bool {
    match (source, target) {
        (ConfigLayerSource::User { file }, ConfigWriteTarget::User) => same_config_path(file, path),
        (ConfigLayerSource::Workspace { file }, ConfigWriteTarget::Workspace) => same_config_path(file, path),
        (ConfigLayerSource::Project { file }, ConfigWriteTarget::Project) => same_config_path(file, path),
        _ => false,
    }
}

fn save_config_for_target(target: ConfigWriteTarget, path: &Path, config: &DiapasonConfig) -> Result<()> {
    let explicit_session_path = explicit_config_path()
        .as_deref()
        .is_some_and(|explicit_path| same_config_path(explicit_path, path));
    let repository_controlled = match target {
        ConfigWriteTarget::Project => true,
        ConfigWriteTarget::Workspace => !explicit_session_path,
        ConfigWriteTarget::User => false,
    };

    if repository_controlled {
        ConfigManager::save_repository_config_to_path(path, config)
    } else {
        ConfigManager::save_config_to_path(path, config)
    }
}

fn same_config_path(left: &Path, right: &Path) -> bool {
    let left = canonicalize(left).unwrap_or_else(|_| normalize_path(left));
    let right = canonicalize(right).unwrap_or_else(|_| normalize_path(right));
    left == right
}

fn path_entry_exists(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn load_or_default_toml(path: &Path) -> Result<TomlValue> {
    if !path.exists() {
        return Ok(TomlValue::Table(toml::Table::new()));
    }

    let content = fs::read_to_string(path).with_context(|| format!("Failed to read config file {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("Failed to parse config file {}", path.display()))
}

fn apply_write(root: &mut TomlValue, path: &str, value: &TomlValue, strategy: ConfigWriteStrategy) -> Result<()> {
    let existing = get_or_create_path_mut(root, path)?;
    match strategy {
        ConfigWriteStrategy::Replace => {
            *existing = value.clone();
        }
        ConfigWriteStrategy::Upsert => {
            if existing.is_table() && value.is_table() {
                merge_toml_values(existing, value);
            } else {
                *existing = value.clone();
            }
        }
    }
    Ok(())
}

fn get_or_create_path_mut<'a>(root: &'a mut TomlValue, path: &str) -> Result<&'a mut TomlValue> {
    let mut current = root;
    let parts: Vec<&str> = path.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        bail!("Invalid empty config path");
    }

    for (index, part) in parts.iter().enumerate() {
        let is_last = index == parts.len() - 1;
        let table = current
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("Path '{path}' traverses non-table value"))?;

        if is_last {
            let entry = table
                .entry((*part).to_string())
                .or_insert_with(|| TomlValue::Table(toml::Table::new()));
            return Ok(entry);
        }

        current = table
            .entry((*part).to_string())
            .or_insert_with(|| TomlValue::Table(toml::Table::new()));
    }

    bail!("Could not resolve config path '{path}'")
}

fn get_value_by_path<'a>(root: &'a TomlValue, path: &str) -> Option<&'a TomlValue> {
    let mut current = root;
    for part in path.split('.').filter(|part| !part.is_empty()) {
        let table = current.as_table()?;
        current = table.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use diapason_commons::reference::StaticWorkspacePaths;
    use diapason_config::defaults::WorkspacePathsDefaults;
    use diapason_config::defaults::provider::with_config_defaults_provider_for_test;
    use diapason_config::loader::set_explicit_config_path;
    use serial_test::serial;

    #[test]
    #[serial]
    fn read_returns_layers_and_origins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let home_config = workspace.join("home").join("config.toml");
        let workspace_config = workspace.join("config.toml");
        fs::create_dir_all(home_config.parent().expect("home parent")).expect("home dir");

        fs::write(&home_config, "agent.provider = \"openai\"\n").expect("home config");
        fs::write(&workspace_config, "agent.provider = \"anthropic\"\nagent.default_model = \"claude-sonnet-4-6\"\n")
            .expect("workspace config");

        let static_paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths)).with_home_paths(vec![home_config]);

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::read(ConfigReadRequest {
                workspace: workspace.to_path_buf(),
                runtime_overrides: Vec::new(),
            })
            .expect("read response");

            assert!(!response.layers.is_empty());
            assert!(!response.merged_version.is_empty());
            assert!(response.origins.contains_key("agent.provider"));
        });
    }

    #[test]
    #[serial]
    fn write_reports_override_when_higher_layer_wins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let home_config = workspace.join("home").join("config.toml");
        let workspace_config = workspace.join("config.toml");
        fs::create_dir_all(home_config.parent().expect("home parent")).expect("home dir");

        fs::write(&home_config, "agent.provider = \"openai\"\n").expect("home config");
        fs::write(&workspace_config, "agent.provider = \"gemini\"\n").expect("workspace config");

        let static_paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths)).with_home_paths(vec![home_config]);

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::write(ConfigWriteRequest {
                workspace: workspace.to_path_buf(),
                target: ConfigWriteTarget::User,
                path: "agent.provider".to_string(),
                value: TomlValue::String("anthropic".to_string()),
                strategy: ConfigWriteStrategy::Replace,
                expected_layer_version: None,
            })
            .expect("write response");

            assert_eq!(response.effective_value, Some(TomlValue::String("gemini".to_string())));
            assert!(response.overridden_metadata.is_some());
        });
    }

    #[test]
    #[serial]
    fn repository_target_write_does_not_persist_provider_endpoint_or_credentials() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let home_config = workspace.join("home/config.toml");
        let workspace_config = workspace.join("config.toml");
        fs::create_dir_all(home_config.parent().expect("home parent")).expect("home dir");

        fs::write(
            &home_config,
            "[provider_overrides.openai]\nbase_url = \"https://trusted.example/v1\"\napi_key_env = \"TRUSTED_API_KEY\"\n",
        )
        .expect("home config");
        fs::write(&workspace_config, "agent.provider = \"openai\"\n").expect("workspace config");

        let static_paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(vec![home_config])
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::write(ConfigWriteRequest {
                workspace: workspace.to_path_buf(),
                target: ConfigWriteTarget::Workspace,
                path: "provider_overrides.openai.base_url".to_string(),
                value: TomlValue::String("https://attacker.example/v1".to_string()),
                strategy: ConfigWriteStrategy::Replace,
                expected_layer_version: None,
            })
            .expect("repository write should complete");

            assert_eq!(response.effective_value, Some(TomlValue::String("https://trusted.example/v1".to_string())));
            let workspace_content = fs::read_to_string(&workspace_config).expect("workspace config");
            assert!(!workspace_content.contains("attacker.example"));
            assert!(!workspace_content.contains("base_url"));
            assert!(!workspace_content.contains("api_key_env"));
        });
    }

    #[test]
    #[serial]
    fn user_writes_target_the_canonical_path_not_the_legacy_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let legacy_path = workspace.join("legacy").join("config.toml");
        let canonical_path = workspace.join("xdg").join("config.toml");
        fs::create_dir_all(legacy_path.parent().expect("legacy parent")).expect("legacy dir");
        fs::write(&legacy_path, "agent.provider = \"openai\"\n").expect("legacy config");

        let static_paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(vec![legacy_path.clone(), canonical_path.clone()]);

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            ConfigService::write(ConfigWriteRequest {
                workspace: workspace.to_path_buf(),
                target: ConfigWriteTarget::User,
                path: "agent.provider".to_string(),
                value: TomlValue::String("anthropic".to_string()),
                strategy: ConfigWriteStrategy::Replace,
                expected_layer_version: None,
            })
            .expect("write response");

            assert!(canonical_path.exists(), "the canonical user config should be created");
            assert_eq!(fs::read_to_string(&legacy_path).expect("legacy config"), "agent.provider = \"openai\"\n");
        });
    }

    #[test]
    #[serial]
    fn write_rejects_stale_expected_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let workspace_config = workspace.join("config.toml");
        fs::write(&workspace_config, "agent.provider = \"openai\"\n\n[workspace]\nuse_root_config = true\n")
            .expect("workspace config");

        let response = ConfigService::write(ConfigWriteRequest {
            workspace: workspace.to_path_buf(),
            target: ConfigWriteTarget::Workspace,
            path: "agent.provider".to_string(),
            value: TomlValue::String("anthropic".to_string()),
            strategy: ConfigWriteStrategy::Replace,
            expected_layer_version: Some("stale-version".to_string()),
        });

        assert!(response.is_err());
        let error = format!("{:#}", response.expect_err("expected stale version error"));
        assert!(error.contains("Layer version mismatch"));
    }

    #[test]
    #[serial]
    fn reset_workspace_layer_preserves_lower_precedence_user_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let user_config = workspace.join("home").join("config.toml");
        let workspace_config = workspace.join("config.toml");
        fs::create_dir_all(user_config.parent().expect("user parent")).expect("user dir");
        fs::write(&user_config, "agent.provider = \"openai\"\n").expect("user config");
        fs::write(&workspace_config, "agent.provider = \"anthropic\"\n").expect("workspace config");

        let static_paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths)).with_home_paths(vec![user_config]);

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::reset(ConfigResetRequest {
                workspace: workspace.to_path_buf(),
                target: ConfigWriteTarget::Workspace,
                expected_layer_version: None,
                path: None,
            })
            .expect("workspace reset");

            assert_eq!(response.target, ConfigWriteTarget::Workspace);
            assert!(response.had_file);
            assert!(fs::read_to_string(&workspace_config).expect("reset file").trim().is_empty());

            let effective: DiapasonConfig =
                serde_json::from_value(response.effective_config).expect("effective config");
            assert_eq!(effective.agent.provider, "openai");
        });
    }

    #[test]
    #[serial]
    fn reset_canonical_user_layer_preserves_legacy_user_layer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path();
        let legacy_config = workspace.join("legacy").join("config.toml");
        let canonical_config = workspace.join("canonical").join("config.toml");
        fs::create_dir_all(legacy_config.parent().expect("legacy parent")).expect("legacy dir");
        fs::create_dir_all(canonical_config.parent().expect("canonical parent")).expect("canonical dir");
        fs::write(&legacy_config, "agent.provider = \"openai\"\n").expect("legacy config");
        fs::write(&canonical_config, "agent.provider = \"anthropic\"\n").expect("canonical config");

        let static_paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(vec![legacy_config.clone(), canonical_config.clone()]);

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::reset(ConfigResetRequest {
                workspace: workspace.to_path_buf(),
                target: ConfigWriteTarget::User,
                expected_layer_version: None,
                path: None,
            })
            .expect("user reset");

            assert_eq!(response.path, canonical_config);
            assert!(canonical_config.exists());
            assert_eq!(fs::read_to_string(&legacy_config).expect("legacy config"), "agent.provider = \"openai\"\n");
            let effective: DiapasonConfig =
                serde_json::from_value(response.effective_config).expect("effective config");
            assert_eq!(effective.agent.provider, "openai");
        });
    }

    #[test]
    #[serial]
    fn reset_project_layer_preserves_workspace_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let config_dir = workspace.join(diapason_commons::workspace_dir_name());
        let project_dir = config_dir.join("projects").join("demo").join("config");
        fs::create_dir_all(&project_dir).expect("project config dir");
        fs::write(workspace.join(".diapason-project"), "demo\n").expect("project marker");
        fs::write(workspace.join("config.toml"), "agent.provider = \"openai\"\n").expect("workspace config");
        let project_config = project_dir.join("config.toml");
        fs::write(&project_config, "agent.default_model = \"project-model\"\n").expect("project config");

        let static_paths = StaticWorkspacePaths::new(&workspace, &config_dir);
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(Vec::new())
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::reset(ConfigResetRequest {
                workspace: workspace.clone(),
                target: ConfigWriteTarget::Project,
                expected_layer_version: None,
                path: None,
            })
            .expect("project reset");

            assert_eq!(response.target, ConfigWriteTarget::Project);
            assert_eq!(response.path, project_config);
            assert!(
                fs::read_to_string(&project_config)
                    .expect("reset project file")
                    .trim()
                    .is_empty()
            );
            let effective: DiapasonConfig =
                serde_json::from_value(response.effective_config).expect("effective config");
            assert_eq!(effective.agent.provider, "openai");
        });
    }

    #[test]
    #[serial]
    fn reset_workspace_target_uses_explicit_session_config_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace dir");
        let explicit_path = temp.path().join("night.toml");
        fs::write(&explicit_path, "agent.provider = \"openai\"\n").expect("explicit config");

        let static_paths =
            StaticWorkspacePaths::new(&workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(Vec::new())
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            struct ExplicitConfigGuard(Option<PathBuf>);

            impl Drop for ExplicitConfigGuard {
                fn drop(&mut self) {
                    set_explicit_config_path(self.0.take());
                }
            }

            let _override = ExplicitConfigGuard(explicit_config_path());
            set_explicit_config_path(Some(explicit_path.clone()));
            let response = ConfigService::reset(ConfigResetRequest {
                workspace,
                target: ConfigWriteTarget::Workspace,
                expected_layer_version: None,
                path: None,
            })
            .expect("explicit config reset");

            assert_eq!(response.target, ConfigWriteTarget::Workspace);
            assert_eq!(response.path, explicit_path);
            assert!(
                fs::read_to_string(response.path)
                    .expect("reset explicit file")
                    .trim()
                    .is_empty()
            );
        });
    }

    #[test]
    #[serial]
    fn reset_creates_missing_explicit_session_config_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace dir");
        let explicit_path = temp.path().join("new-session.toml");

        let static_paths =
            StaticWorkspacePaths::new(&workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(Vec::new())
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            struct ExplicitConfigGuard(Option<PathBuf>);

            impl Drop for ExplicitConfigGuard {
                fn drop(&mut self) {
                    set_explicit_config_path(self.0.take());
                }
            }

            let _override = ExplicitConfigGuard(explicit_config_path());
            set_explicit_config_path(Some(explicit_path.clone()));
            let response = ConfigService::reset(ConfigResetRequest {
                workspace,
                target: ConfigWriteTarget::Workspace,
                expected_layer_version: None,
                path: None,
            })
            .expect("missing explicit config reset");

            assert!(!response.had_file);
            assert_eq!(fs::read_to_string(&explicit_path).expect("created config"), "");
        });
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn reset_rejects_symlinked_workspace_config() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside.toml");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(&outside, "agent.provider = \"openai\"\n").expect("outside config");
        symlink(&outside, workspace.join("config.toml")).expect("config symlink");

        let response = ConfigService::reset(ConfigResetRequest {
            workspace,
            target: ConfigWriteTarget::Workspace,
            expected_layer_version: None,
            path: None,
        });

        let error = format!("{:#}", response.expect_err("symlink reset must fail"));
        assert!(error.contains("symlink"));
        assert!(outside.exists(), "the symlink target must remain untouched");
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn reset_rejects_dangling_workspace_symlink_instead_of_using_fallback() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let config_dir = workspace.join(diapason_commons::workspace_dir_name());
        let fallback = config_dir.join("config.toml");
        let dangling_target = temp.path().join("missing.toml");
        fs::create_dir_all(&config_dir).expect("workspace config dir");
        fs::write(&fallback, "agent.provider = \"openai\"\n").expect("fallback config");
        symlink(&dangling_target, workspace.join("config.toml")).expect("dangling config symlink");

        let static_paths = StaticWorkspacePaths::new(&workspace, &config_dir);
        let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
            .with_home_paths(Vec::new())
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let response = ConfigService::reset(ConfigResetRequest {
                workspace: workspace.clone(),
                target: ConfigWriteTarget::Workspace,
                expected_layer_version: None,
                path: None,
            });

            let error = format!("{:#}", response.expect_err("dangling symlink reset must fail"));
            assert!(error.contains("symlink"));
            assert_eq!(fs::read_to_string(&fallback).expect("fallback config"), "agent.provider = \"openai\"\n");
        });
    }
}
