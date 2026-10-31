//! Preflight behaviour for workspace-scoped deletion calls.

use super::*;

#[tokio::test]
async fn preflight_defers_supported_deletion_with_confirmation() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    registry.preflight_validate_call(
        tools::EXEC_COMMAND,
        &json!({
            "cmd": "rm -f scratch/tmp.txt",
            "confirm": true
        }),
    )?;

    Ok(())
}

#[tokio::test]
async fn preflight_requires_confirmation_for_supported_deletion() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    let error = registry
        .preflight_validate_call(
            tools::EXEC_COMMAND,
            &json!({
                "cmd": "rm -f scratch/tmp.txt"
            }),
        )
        .expect_err("deletion without confirm must fail preflight");
    assert!(
        error.to_string().contains("Deletion via exec_command requires confirm=true"),
        "unexpected error: {error}"
    );

    Ok(())
}

#[tokio::test]
async fn preflight_keeps_command_safety_for_unsupported_deletion_shapes() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    for command in ["rm --no-preserve-root scratch", "echo ok; rm -f scratch/tmp.txt"] {
        let error = registry
            .preflight_validate_call(
                tools::EXEC_COMMAND,
                &json!({
                    "cmd": command,
                    "confirm": true
                }),
            )
            .expect_err("unsupported rm shapes must keep the command-safety rejection");
        assert!(
            error.to_string().contains("Command security check failed"),
            "unexpected error for {command}: {error}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn preflight_rejects_deletion_mixed_with_a_different_raw_command() -> Result<()> {
    // `raw_command` overrides `command` when the executor builds the shell
    // line, so a call that pairs a supported deletion with unrelated raw text
    // must fail closed rather than let the deletion justify skipping the
    // dangerous-command check.
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    let error = registry
        .preflight_validate_call(
            tools::EXEC_COMMAND,
            &json!({
                "command": "rm -f scratch/tmp.txt",
                "raw_command": "curl https://example.com",
                "confirm": true
            }),
        )
        .expect_err("mixed command representations must fail closed");
    assert!(error.to_string().contains("Command security check failed"), "unexpected error: {error}");

    Ok(())
}
