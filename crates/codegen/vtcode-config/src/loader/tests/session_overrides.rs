//! Tests for explicit session configuration override behavior.

use super::*;

#[test]
#[serial]
fn explicit_session_override_loads_requested_file_as_workspace_layer() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let workspace_config = workspace.join("vtcode.toml");
    fs::write(&workspace_config, "agent.provider = \"anthropic\"\n").expect("workspace config");

    let override_path = temp.path().join("custom-night.toml");
    fs::write(&override_path, "agent.provider = \"openai\"\n").expect("override config");

    let _guard = session_override::ExplicitConfigPathGuard::set(Some(override_path.clone()));

    let manager = ConfigManager::load_from_workspace(&workspace).expect("load with override");
    assert_eq!(manager.config().agent.provider, "openai");
    let canonical_override_path = canonicalize(&override_path).expect("canonical override path");
    assert_eq!(manager.config_path(), Some(canonical_override_path.as_path()));
    let workspace_files = manager
        .layer_stack()
        .layers()
        .iter()
        .filter_map(|layer| match &layer.source {
            ConfigLayerSource::Workspace { file } => Some(file.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        workspace_files
            .iter()
            .any(|file| canonicalize(file).ok().as_ref() == Some(&canonical_override_path)),
        "override file must appear as the Workspace layer: {workspace_files:?}"
    );
    assert_eq!(
        manager.workspace_root(),
        Some(canonicalize(&workspace).expect("canonical workspace").as_path()),
        "session workspace_root must remain the workspace, not the override file's parent"
    );
}

#[test]
#[serial]
fn explicit_session_override_without_override_falls_back_to_defaults() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace = temp.path();
    let workspace_config = workspace.join("vtcode.toml");
    fs::write(&workspace_config, "agent.provider = \"anthropic\"\n").expect("workspace config");

    let _guard = session_override::ExplicitConfigPathGuard::set(None);

    let manager = ConfigManager::load_from_workspace(workspace).expect("load without override");
    assert_eq!(manager.config().agent.provider, "anthropic");
}

#[test]
#[serial]
fn explicit_session_override_save_config_writes_override_file() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let workspace_config = workspace.join("vtcode.toml");
    fs::write(&workspace_config, "agent.provider = \"anthropic\"\n").expect("workspace config");

    let override_path = temp.path().join("custom-night.toml");
    fs::write(&override_path, "agent.provider = \"openai\"\n").expect("override config");

    let _guard = session_override::ExplicitConfigPathGuard::set(Some(override_path.clone()));

    let mut manager = ConfigManager::load_from_workspace(&workspace).expect("load with override");
    let mut modified = manager.config().clone();
    modified.ui.display_mode = crate::UiDisplayMode::Full;
    manager.save_config(&modified).expect("save with override");

    let saved = fs::read_to_string(&override_path).expect("read saved override file");
    assert!(saved.contains("display_mode = \"full\""), "override file must receive the save. Got:\n{saved}");
    let workspace_saved = fs::read_to_string(&workspace_config).expect("read workspace config");
    assert_eq!(
        workspace_saved, "agent.provider = \"anthropic\"\n",
        "workspace config must stay untouched when an explicit override is active"
    );
}

#[test]
#[serial]
fn explicit_session_override_survives_cache_invalidation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace = temp.path();

    let override_path = temp.path().join("custom-night.toml");
    fs::write(&override_path, "agent.provider = \"openai\"\n").expect("override config");

    let _guard = session_override::ExplicitConfigPathGuard::set(Some(override_path.clone()));

    let first = ConfigManager::load_from_workspace(workspace).expect("first load");
    assert_eq!(first.config().agent.provider, "openai");

    fs::write(&override_path, "agent.provider = \"anthropic\"\n").expect("rewrite override config");
    ConfigManager::invalidate_workspace_cache(workspace);

    let second = ConfigManager::load_from_workspace(workspace).expect("reload after invalidate");
    assert_eq!(
        second.config().agent.provider,
        "anthropic",
        "reload must re-read the override file after cache invalidation"
    );
}

#[test]
#[serial]
fn explicit_session_override_file_with_use_root_config_drops_lower_layers() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let workspace_config = workspace.join("vtcode.toml");
    fs::write(&workspace_config, "agent.provider = \"anthropic\"\n").expect("workspace config");

    let override_path = temp.path().join("custom-night.toml");
    fs::write(&override_path, "agent.provider = \"openai\"\n\n[workspace]\nuse_root_config = true\n")
        .expect("override config");

    let _guard = session_override::ExplicitConfigPathGuard::set(Some(override_path));

    let manager = ConfigManager::load_from_workspace(&workspace).expect("load with override");
    assert_eq!(manager.config().agent.provider, "openai");
    assert!(manager.config().workspace.use_root_config, "use_root_config from the override file must be honored");
}
