#![allow(
    clippy::panic_in_result_fn,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use anyhow::Result;
use serde_json::json;
use vtcode_core::config::constants::tools;

mod support;
use support::TestHarness;

#[tokio::test]
async fn run_command_uses_pty_backend() -> Result<()> {
    let harness = TestHarness::new()?;
    let _sample_path = harness.write_file("sample.txt", "hello")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let response = registry
        .execute_tool(
            tools::RUN_PTY_CMD,
            json!({
                "command": "ls",
                "working_dir": "."
            }),
        )
        .await?;
    // debug: response logged in test harness if needed

    assert_eq!(response["success"], true);
    // Check for PTY-related fields (implementation may vary)
    let has_pty_indicators = response.get("session_id").is_some()
        || response.get("process_id").is_some()
        || response.get("pty_enabled").is_some()
        || response.get("mode").is_some();
    assert!(has_pty_indicators, "Response should have PTY-related fields. Response: {response:?}");

    let output = response["output"].as_str().unwrap_or_default();
    let stdout = response["stdout"].as_str().unwrap_or_default();
    let combined_output = format!("{output} {stdout}");
    assert!(
        combined_output.contains("sample.txt"),
        "Output should contain sample.txt. output='{output}', stdout='{stdout}'"
    );

    Ok(())
}

#[tokio::test]
async fn exec_pty_cmd_resolves_and_runs_in_registry() -> Result<()> {
    let harness = TestHarness::new()?;
    let _sample_path = harness.write_file("sample_pty.txt", "hello")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    // EXEC_PTY_CMD is offered to the model but must also resolve to a real
    // executor; otherwise the call fails with "not found in registry".
    let response = registry
        .execute_tool(
            tools::EXEC_PTY_CMD,
            json!({
                "cmd": "ls",
                "workdir": "."
            }),
        )
        .await?;

    assert_eq!(response["success"], true, "exec_pty_cmd must execute; response was {response:?}");
    let output = response["output"].as_str().unwrap_or_default();
    let stdout = response["stdout"].as_str().unwrap_or_default();
    let combined_output = format!("{output} {stdout}");
    assert!(
        combined_output.contains("sample_pty.txt"),
        "Output should contain sample_pty.txt. output='{output}', stdout='{stdout}'"
    );

    Ok(())
}

#[tokio::test]
async fn run_command_accepts_indexed_arguments_zero_based() -> Result<()> {
    let harness = TestHarness::new()?;
    let _sample_path = harness.write_file("sample.txt", "hello")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let response = registry
        .execute_tool(
            tools::RUN_PTY_CMD,
            json!({
                "command.0": "ls",
                "command.1": "-a",
                "working_dir": "."
            }),
        )
        .await?;

    eprintln!("Response: {response:?}");
    assert_eq!(response["success"], true);
    let output = response["output"].as_str().unwrap_or_default();
    let stdout = response["stdout"].as_str().unwrap_or_default();
    let combined_output = format!("{output} {stdout}");
    assert!(
        combined_output.contains("sample.txt"),
        "Output should contain sample.txt. output='{output}', stdout='{stdout}'"
    );

    Ok(())
}

#[tokio::test]
async fn run_command_accepts_indexed_arguments_one_based() -> Result<()> {
    let harness = TestHarness::new()?;
    let _sample_path = harness.write_file("sample2.txt", "hello2")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let response = registry
        .execute_tool(
            tools::RUN_PTY_CMD,
            json!({
                "command.1": "ls",
                "command.2": "-a",
                "working_dir": "."
            }),
        )
        .await?;

    assert_eq!(response["success"], true);
    let output = response["output"].as_str().unwrap_or_default();
    let stdout = response["stdout"].as_str().unwrap_or_default();
    let combined_output = format!("{output} {stdout}");
    assert!(
        combined_output.contains("sample2.txt"),
        "Output should contain sample2.txt. output='{output}', stdout='{stdout}'"
    );

    Ok(())
}

/// Render either an execution error or a tool-level failure payload so tests
/// can assert on the refusal reason without depending on the error channel.
fn rejection_text(result: Result<serde_json::Value>) -> String {
    match result {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    }
}

#[tokio::test]
async fn exec_command_deletes_confirmed_workspace_file() -> Result<()> {
    let harness = TestHarness::new()?;
    let scratch = harness.write_file("scratch/tmp.txt", "scratch")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let response = registry
        .execute_tool(
            tools::EXEC_COMMAND,
            json!({
                "cmd": "rm -f scratch/tmp.txt",
                "confirm": true
            }),
        )
        .await?;

    assert_eq!(response["success"], true, "confirmed deletion should run: {response}");
    assert!(!scratch.exists(), "confirmed deletion should remove the scratch file");

    Ok(())
}

#[tokio::test]
async fn exec_command_rejects_deletion_without_confirmation() -> Result<()> {
    let harness = TestHarness::new()?;
    let scratch = harness.write_file("scratch/tmp.txt", "scratch")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let text = rejection_text(
        registry
            .execute_tool(
                tools::EXEC_COMMAND,
                json!({
                    "cmd": "rm -f scratch/tmp.txt"
                }),
            )
            .await,
    );

    assert!(text.contains("confirm=true"), "unexpected refusal: {text}");
    assert!(scratch.exists(), "rejected deletion must leave the scratch file in place");

    Ok(())
}

#[tokio::test]
async fn exec_command_rejects_workspace_root_deletion() -> Result<()> {
    let harness = TestHarness::new()?;
    let _keep = harness.write_file("keep.txt", "keep")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let text = rejection_text(
        registry
            .execute_tool(
                tools::EXEC_COMMAND,
                json!({
                    "cmd": "rm -f .",
                    "confirm": true
                }),
            )
            .await,
    );

    assert!(text.contains("workspace") || text.contains("current directory"), "unexpected refusal: {text}");
    assert!(harness.workspace().join("keep.txt").exists());

    Ok(())
}

#[tokio::test]
async fn exec_command_rejects_traversal_escape() -> Result<()> {
    let harness = TestHarness::new()?;
    let outside = tempfile::TempDir::new()?;
    let sentinel = outside.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"sentinel")?;
    let outside_name = outside.path().file_name().expect("temp dir has a file name");
    let relative = format!("../{}/sentinel.txt", outside_name.to_string_lossy());
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let text = rejection_text(
        registry
            .execute_tool(
                tools::EXEC_COMMAND,
                json!({
                    "cmd": format!("rm -f {relative}"),
                    "confirm": true
                }),
            )
            .await,
    );

    assert!(
        text.contains("outside the workspace") || text.contains("refusing to delete"),
        "unexpected refusal: {text}"
    );
    assert!(sentinel.exists(), "traversal deletion must not touch the outside file");

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_command_rejects_symlink_escape() -> Result<()> {
    let harness = TestHarness::new()?;
    let outside = tempfile::TempDir::new()?;
    let sentinel = outside.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"sentinel")?;
    std::os::unix::fs::symlink(outside.path(), harness.workspace().join("escape"))?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let text = rejection_text(
        registry
            .execute_tool(
                tools::EXEC_COMMAND,
                json!({
                    "cmd": "rm -f escape/sentinel.txt",
                    "confirm": true
                }),
            )
            .await,
    );

    assert!(
        text.contains("outside the workspace") || text.contains("refusing to delete"),
        "unexpected refusal: {text}"
    );
    assert!(sentinel.exists(), "symlink deletion must not touch the outside file");

    Ok(())
}

#[tokio::test]
async fn exec_command_rejects_glob_chained_and_protected_deletions() -> Result<()> {
    let harness = TestHarness::new()?;
    let scratch = harness.write_file("scratch/tmp.txt", "scratch")?;
    let git_config = harness.write_file(".git/config", "[core]")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    for command in [
        "rm -f scratch/*",
        "rm -f scratch/tmp.txt; echo done",
        "rm -f .git/config",
    ] {
        let text = rejection_text(
            registry
                .execute_tool(
                    tools::EXEC_COMMAND,
                    json!({
                        "cmd": command,
                        "confirm": true
                    }),
                )
                .await,
        );
        assert!(
            text.contains("not supported")
                || text.contains("protected")
                || text.contains("refusing to delete")
                || text.contains("Dotfile")
                || text.contains("Command security check failed"),
            "unexpected refusal for {command}: {text}"
        );
    }

    assert!(scratch.exists(), "glob deletion must leave the scratch file in place");
    assert!(git_config.exists(), "protected metadata deletion must leave the file in place");

    Ok(())
}

#[tokio::test]
async fn exec_command_rejects_deletion_mixed_with_a_different_raw_command() -> Result<()> {
    let harness = TestHarness::new()?;
    let scratch = harness.write_file("scratch/tmp.txt", "scratch")?;
    let registry = harness.registry().await;
    registry.allow_all_tools().await?;

    let text = rejection_text(
        registry
            .execute_tool(
                tools::EXEC_COMMAND,
                json!({
                    "command": "rm -f scratch/tmp.txt",
                    "raw_command": "curl https://example.com",
                    "confirm": true
                }),
            )
            .await,
    );
    assert!(
        text.contains("not supported") || text.contains("Command security check failed"),
        "mixed command representations must fail closed: {text}"
    );
    assert!(scratch.exists(), "a refused mixed call must leave the scratch file in place");

    Ok(())
}
