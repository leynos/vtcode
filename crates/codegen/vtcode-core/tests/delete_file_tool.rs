//! File and directory deletion through explicitly authorized workspace fixtures.

#[path = "support/config_defaults.rs"]
mod config_defaults;

use anyhow::Result;
use serde_json::json;
use vtcode_config::CommandsConfig;
use vtcode_config::constants::tools;
use vtcode_core::tool_policy::ToolPolicyManager;
use vtcode_core::tools::ToolRegistry;

async fn fixture_registry(temp: &tempfile::TempDir, commands_config: Option<&CommandsConfig>) -> Result<ToolRegistry> {
    let policy_path = temp.path().join(".vtcode/test-tool-policy.json");
    let policy_manager = ToolPolicyManager::new_with_config_path(policy_path).await?;
    let registry = ToolRegistry::new_with_custom_policy(temp.path().to_path_buf(), policy_manager).await;
    if let Some(commands_config) = commands_config {
        registry.apply_commands_config(commands_config);
    }
    registry.initialize_async().await?;
    registry.allow_all_tools().await?;
    Ok(registry)
}

fn recursive_delete_fixture_commands_config() -> CommandsConfig {
    let mut config = CommandsConfig::default();
    config.allow_list.clear();
    config.allow_glob.clear();
    config.allow_regex = vec![r"^rm -rf nested$".to_owned()];
    config.deny_list.retain(|command| command != "rm");
    config.deny_glob.retain(|command| command != "rm *");
    config.deny_regex.retain(|pattern| pattern != r"rm\s+(-rf|--force)");
    config
}

#[tokio::test]
async fn delete_file_tool_removes_file() {
    let tmp = tempfile::TempDir::new().expect("create temporary test workspace");
    let _config_defaults = config_defaults::IsolatedConfigDefaultsGuard::install(tmp.path()).await;
    let file_path = tmp.path().join("to_delete.txt");
    tokio::fs::write(&file_path, b"hello")
        .await
        .expect("write file deletion fixture");

    let registry = fixture_registry(&tmp, None).await.expect("create isolated file tool registry");

    // Ensure file exists
    assert!(file_path.exists());

    let args = json!({
        "input": "*** Begin Patch\n*** Delete File: to_delete.txt\n*** End Patch"
    });
    let val = registry
        .execute_tool(tools::APPLY_PATCH, args)
        .await
        .expect("delete fixture file through apply_patch");
    assert_eq!(val.get("success").and_then(|v| v.as_bool()), Some(true));
    // Verify removal
    assert!(!file_path.exists());
}

#[tokio::test]
async fn delete_file_tool_removes_directory_recursively() {
    let tmp = tempfile::TempDir::new().expect("create temporary test workspace");
    let _config_defaults = config_defaults::IsolatedConfigDefaultsGuard::install(tmp.path()).await;
    let dir_path = tmp.path().join("nested");
    let child_path = dir_path.join("file.txt");
    tokio::fs::create_dir_all(&dir_path)
        .await
        .expect("create nested deletion fixture");
    tokio::fs::write(&child_path, b"hi")
        .await
        .expect("write nested deletion fixture");

    let commands_config = recursive_delete_fixture_commands_config();
    let registry = fixture_registry(&tmp, Some(&commands_config))
        .await
        .expect("create isolated recursive-delete registry");

    let val = registry
        .execute_harness_command_session(json!({
            "action": "run",
            "command": "rm -rf nested",
            "confirm": true
        }))
        .await
        .expect("delete nested fixture through the command session");

    assert_eq!(val.get("success").and_then(|v| v.as_bool()), Some(true));
    assert!(!dir_path.exists());
}
