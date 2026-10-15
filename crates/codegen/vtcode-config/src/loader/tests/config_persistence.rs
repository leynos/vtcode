//! Tests for configuration persistence and save behavior.

use super::*;

fn assert_saved_comments_are_preserved(saved_content: &str) {
    assert!(saved_content.contains("# This is a test comment"), "top-level comment should be preserved");
    assert!(saved_content.contains("# Provider comment"), "inline comment should be preserved");
    assert!(saved_content.contains("# Tools section comment"), "section comment should be preserved");
}

fn assert_saved_model_is_updated(saved_content: &str) {
    assert!(saved_content.contains("gpt-5"), "modified value should be present");
}

fn assert_repository_config_excludes_trusted_provider_settings(saved: &toml::Value) {
    assert!(saved.get("custom_providers").is_none(), "custom providers must stay in trusted layers");
    let openai_override = saved
        .get("provider_overrides")
        .and_then(|value| value.get("openai"))
        .expect("model-list override should remain");
    assert!(openai_override.get("base_url").is_none(), "repository endpoint must be removed");
    assert!(openai_override.get("api_key_env").is_none(), "repository credential selector must be removed");
}

fn assert_repository_config_preserves_model_values(saved: &toml::Value) {
    let openai_models = saved
        .get("provider_overrides")
        .and_then(|value| value.get("openai"))
        .and_then(|value| value.get("models"));
    assert_eq!(openai_models.and_then(toml::Value::as_array).map(Vec::len), Some(1));
    assert_eq!(
        saved
            .get("agent")
            .and_then(|value| value.get("default_model"))
            .and_then(toml::Value::as_str),
        Some("new-model")
    );
}

fn assert_trusted_provider_is_loaded(config: &VTCodeConfig) {
    assert_eq!(config.custom_providers.len(), 1);
    assert_eq!(config.provider_overrides["openai"].base_url.as_deref(), Some("https://trusted.example/v1"));
}

fn assert_workspace_save_excludes_trusted_values(written: &str) {
    assert!(!written.contains("custom_providers"), "workspace must not receive trusted providers");
    assert!(!written.contains("trusted.example"), "workspace must not receive trusted provider endpoints");
    assert!(!written.contains("TRUSTED_OPENAI_API_KEY"), "workspace must not receive credential selectors");
}

fn assert_workspace_save_persists_theme(written: &str) {
    assert!(written.contains("theme = \"ansi\""), "workspace setting should still be persisted");
}

fn assert_trusted_provider_survives_workspace_reload(config: &VTCodeConfig) {
    assert_eq!(config.custom_providers[0].name, "trusted-provider");
    assert_eq!(config.provider_overrides["openai"].base_url.as_deref(), Some("https://trusted.example/v1"));
    assert_eq!(config.agent.theme, "ansi");
}

#[test]
fn save_config_preserves_comments() {
    use std::io::Write;

    let mut temp_file = NamedTempFile::new().expect("failed to create temp file");
    let config_with_comments = r#"# This is a test comment
[agent]
# Provider comment
provider = "anthropic"
default_model = "gpt-5-nano"

# Tools section comment
[tools]
default_policy = "deny"
"#;

    write!(temp_file, "{config_with_comments}").expect("failed to write temp config");
    temp_file.flush().expect("failed to flush");

    // Load config
    let manager = ConfigManager::load_from_file(temp_file.path()).expect("failed to load config");

    // Modify and save
    let mut modified_config = manager.config().clone();
    modified_config.agent.default_model = "gpt-5".to_string();

    ConfigManager::save_config_to_path(temp_file.path(), &modified_config).expect("failed to save config");

    // Read back and verify comments are preserved
    let saved_content = fs::read_to_string(temp_file.path()).expect("failed to read saved config");
    assert_saved_comments_are_preserved(&saved_content);
    assert_saved_model_is_updated(&saved_content);
}

#[test]
#[serial]
fn config_defaults_provider_overrides_paths_and_theme() {
    let workspace = assert_fs::TempDir::new().expect("failed to create workspace");
    let workspace_root = workspace.path();
    let config_dir = workspace_root.join("config-root");
    fs::create_dir_all(&config_dir).expect("failed to create config directory");

    let config_file_name = "custom-config.toml";
    let config_path = config_dir.join(config_file_name);
    let serialized = toml::to_string(&VTCodeConfig::default()).expect("failed to serialize default config");
    fs::write(&config_path, serialized).expect("failed to write config file");

    let static_paths = StaticWorkspacePaths::new(workspace_root, &config_dir);
    let provider = WorkspacePathsDefaults::new(Arc::new(static_paths))
        .with_config_file_name(config_file_name)
        .with_home_paths(Vec::new())
        .with_syntax_theme("custom-theme")
        .with_syntax_languages(vec!["zig".to_string()]);

    defaults::provider::with_config_defaults_provider_for_test(Arc::new(provider), || {
        let manager = ConfigManager::load_from_workspace(workspace_root).expect("failed to load workspace config");

        let resolved_path = manager.config_path().expect("config path should be resolved");
        let resolved_canonical = canonicalize(resolved_path).expect("resolved config path should canonicalize");
        let expected_canonical = canonicalize(&config_path).expect("expected config path should canonicalize");
        assert_eq!(resolved_canonical, expected_canonical);

        assert_eq!(SyntaxHighlightingDefaults::theme(), "custom-theme");
        assert_eq!(SyntaxHighlightingDefaults::enabled_languages(), vec!["zig".to_string()]);
    });
}

#[test]
#[serial]
fn save_config_updates_disk_file() {
    let temp_dir = tempfile::tempdir().expect("failed to create temporary workspace");
    let workspace = temp_dir.path();
    let config_path = workspace.join("vtcode.toml");

    // Write initial config
    let initial_config = r#"
[ui]
display_mode = "minimal"
show_sidebar = false
"#;
    fs::write(&config_path, initial_config).expect("failed to write initial config");

    // Load config
    let mut manager = ConfigManager::load_from_workspace(workspace).expect("failed to load config");
    assert_eq!(manager.config().ui.display_mode, crate::UiDisplayMode::Minimal);

    // Modify config (simulating /config palette changes)
    let mut modified_config = manager.config().clone();
    modified_config.ui.display_mode = crate::UiDisplayMode::Full;
    modified_config.ui.show_sidebar = true;

    // Save config
    manager.save_config(&modified_config).expect("failed to save config");

    // Verify disk file was updated
    let saved_content = fs::read_to_string(&config_path).expect("failed to read saved config");
    assert!(
        saved_content.contains("display_mode = \"full\""),
        "saved config should contain full display_mode. Got:\n{saved_content}"
    );
    assert!(
        !saved_content.contains("show_sidebar"),
        "saved config should prune default show_sidebar. Got:\n{saved_content}"
    );

    // Create a NEW manager to simulate reopening /config palette
    let new_manager = ConfigManager::load_from_workspace(workspace).expect("failed to reload config");
    assert_eq!(
        new_manager.config().ui.display_mode,
        crate::UiDisplayMode::Full,
        "reloaded config should have full display_mode"
    );

    // Force disk read by loading from file directly
    let new_manager2 = ConfigManager::load_from_file(&config_path).expect("failed to reload from file");
    assert!(
        new_manager2.config().ui.show_sidebar,
        "reloaded config should have show_sidebar = true, got: {}",
        new_manager2.config().ui.show_sidebar
    );
}

#[test]
fn repository_config_save_strips_trusted_provider_settings_and_repairs_existing_keys() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("vtcode.toml");
    fs::write(
        &config_path,
        r#"
[agent]
default_model = "old-model"

[[custom_providers]]
name = "stale-provider"
display_name = "Stale Provider"
base_url = "https://stale.example/v1"
model = "stale-model"

[provider_overrides.openai]
models = ["old-model"]
base_url = "https://stale.example/v1"
api_key_env = "STALE_API_KEY"
"#,
    )
    .expect("write stale repository config");

    let config: VTCodeConfig = toml::from_str(
        r#"
[agent]
default_model = "new-model"

[[custom_providers]]
name = "trusted-provider"
display_name = "Trusted Provider"
base_url = "https://trusted.example/v1"
model = "trusted-model"

[provider_overrides.openai]
models = ["trusted-model"]
base_url = "https://trusted.example/v1"
api_key_env = "TRUSTED_API_KEY"
"#,
    )
    .expect("parse trusted effective config");

    ConfigManager::save_repository_config_to_path(&config_path, &config).expect("save repository config");

    let saved: toml::Value = toml::from_str(&fs::read_to_string(&config_path).expect("read repository config"))
        .expect("parse saved repository config");
    assert_repository_config_excludes_trusted_provider_settings(&saved);
    assert_repository_config_preserves_model_values(&saved);
}

#[test]
#[serial]
fn save_config_does_not_flatten_trusted_provider_settings_into_workspace() {
    let workspace = assert_fs::TempDir::new().expect("workspace");
    let user_config = workspace.path().join("home/vtcode.toml");
    fs::create_dir_all(user_config.parent().expect("user config parent")).expect("create user config directory");
    fs::write(
        &user_config,
        r#"
[[custom_providers]]
name = "trusted-provider"
display_name = "Trusted Provider"
base_url = "https://trusted.example/v1"
model = "trusted-model"

[provider_overrides.openai]
models = ["trusted-openai-model"]
base_url = "https://trusted.example/v1"
api_key_env = "TRUSTED_OPENAI_API_KEY"
"#,
    )
    .expect("write user config");
    let workspace_config = workspace.path().join("vtcode.toml");
    fs::write(&workspace_config, "[agent]\ndefault_model = \"workspace-model\"\n").expect("write workspace config");

    let paths = StaticWorkspacePaths::new(workspace.path(), workspace.path().join(".vtcode"));
    let provider = WorkspacePathsDefaults::new(Arc::new(paths))
        .with_home_paths(vec![user_config])
        .with_system_config_paths(Vec::new());

    defaults::provider::with_config_defaults_provider_for_test(Arc::new(provider), || {
        let mut manager = ConfigManager::load_from_workspace(workspace.path()).expect("load layered config");
        assert_trusted_provider_is_loaded(manager.config());

        let mut modified = manager.config().clone();
        modified.agent.theme = "ansi".to_string();
        manager.save_config(&modified).expect("save workspace setting");

        let written = fs::read_to_string(&workspace_config).expect("read workspace config");
        assert_workspace_save_excludes_trusted_values(&written);
        assert_workspace_save_persists_theme(&written);

        let reloaded = ConfigManager::load_from_workspace(workspace.path()).expect("reload layered config");
        assert_trusted_provider_survives_workspace_reload(reloaded.config());
    });
}

#[test]
#[serial]
fn explicitly_selected_config_save_preserves_custom_provider_settings() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let explicit_config = temp_dir.path().join("explicit.toml");
    fs::write(
        &explicit_config,
        r#"
[[custom_providers]]
name = "explicit-provider"
display_name = "Explicit Provider"
base_url = "https://explicit.example/v1"
model = "explicit-model"
"#,
    )
    .expect("write explicit config");

    let paths = StaticWorkspacePaths::new(temp_dir.path(), temp_dir.path().join(".vtcode"));
    let provider = WorkspacePathsDefaults::new(Arc::new(paths))
        .with_home_paths(Vec::new())
        .with_system_config_paths(Vec::new());

    defaults::provider::with_config_defaults_provider_for_test(Arc::new(provider), || {
        let mut manager = ConfigManager::load_from_file(&explicit_config).expect("load explicit config");
        let mut modified = manager.config().clone();
        modified.agent.theme = "ansi".to_string();
        manager.save_config(&modified).expect("save explicit config");

        let saved: toml::Value = toml::from_str(&fs::read_to_string(&explicit_config).expect("read explicit config"))
            .expect("parse explicit config");
        assert_eq!(
            saved
                .get("custom_providers")
                .and_then(toml::Value::as_array)
                .and_then(|providers| providers.first())
                .and_then(|provider| provider.get("name"))
                .and_then(toml::Value::as_str),
            Some("explicit-provider")
        );
        assert!(saved.to_string().contains("theme = \"ansi\""));
    });
}
