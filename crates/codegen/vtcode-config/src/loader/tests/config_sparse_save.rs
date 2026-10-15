//! Tests sparse persistence and migration of deprecated permission keys.

use super::*;

fn assert_sparse_agent_values_are_saved(saved_content: &str) {
    assert!(saved_content.contains("[agent]"));
    assert!(saved_content.contains("default_model = \"gpt-5.6-sol\""));
    assert!(saved_content.contains("theme = \"ansi\""));
}

fn assert_sparse_permission_values_are_saved(saved_content: &str) {
    assert!(saved_content.contains("[permissions]"));
    assert!(saved_content.contains("allow = [\"read_file\"]"));
}

fn assert_sparse_defaults_are_omitted(saved_content: &str) {
    assert!(
        !saved_content.contains("default_primary_agent"),
        "unchanged default primary agent should not be persisted. Got:\n{saved_content}"
    );
    assert!(
        !saved_content.contains("provider = \"openai\""),
        "default agent provider should not be expanded. Got:\n{saved_content}"
    );
    assert!(!saved_content.contains("[ui]"), "default UI section should not be expanded. Got:\n{saved_content}");
}

fn assert_sparse_values_survive_reload(config: &VTCodeConfig) {
    assert_eq!(config.agent.default_model, "gpt-5.6-sol");
    assert_eq!(config.agent.theme, "ansi");
    assert_eq!(config.permissions.allow, vec!["read_file".to_string()]);
}

#[test]
#[serial]
fn save_config_writes_sparse_model_theme_and_permission_values() {
    let temp_dir = tempfile::tempdir().expect("failed to create temporary workspace");
    let workspace = temp_dir.path();
    let config_path = workspace.join("vtcode.toml");
    // Opt out of the developer's global config so this persistence test is
    // deterministic when run on a machine with user-level settings.
    fs::write(&config_path, "[workspace]\nuse_root_config = true\n").expect("failed to write initial config");

    let mut manager = ConfigManager::load_from_workspace(workspace).expect("failed to load config");
    let mut modified_config = manager.config().clone();
    modified_config.agent.default_model = "gpt-5.6-sol".to_string();
    modified_config.agent.theme = "ansi".to_string();
    modified_config.permissions.allow = vec!["read_file".to_string()];

    manager.save_config(&modified_config).expect("failed to save config");

    let saved_content = fs::read_to_string(&config_path).expect("failed to read saved config");
    assert_sparse_agent_values_are_saved(&saved_content);
    assert_sparse_permission_values_are_saved(&saved_content);
    assert_sparse_defaults_are_omitted(&saved_content);

    let reloaded = ConfigManager::load_from_workspace(workspace).expect("failed to reload config");
    assert_sparse_values_survive_reload(reloaded.config());
}

#[test]
#[serial]
fn deprecated_permission_keys_are_migrated_on_save() {
    for deprecated_key in ["allowed_tools", "disallowed_tools"] {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary workspace");
        let workspace = temp_dir.path();
        let config_path = workspace.join("vtcode.toml");
        fs::write(
            &config_path,
            format!(
                r#"
[workspace]
use_root_config = true

[permissions]
{deprecated_key} = ["read_file"]
"#
            ),
        )
        .expect("failed to write config with deprecated permission key");

        // Deprecated keys are accepted on load (they are not a hard error),
        // so existing configs keep working until they are migrated on save.
        let manager = ConfigManager::load_from_workspace(workspace)
            .expect("deprecated permission keys should load before migration");

        // Saving strips the deprecated keys (the save path migrates them away).
        ConfigManager::save_config_to_path(&config_path, manager.config()).expect("failed to save config");

        let written = fs::read_to_string(&config_path).expect("failed to read migrated config");
        assert!(
            !written.contains(deprecated_key),
            "deprecated key {deprecated_key} should be stripped on save. Got:\n{written}"
        );
    }
}
