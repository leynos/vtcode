#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use anyhow::{Context, Result};
use assert_cmd::Command;
use predicates::prelude::*;
use rstest::fixture;

#[path = "../crates/codegen/diapason-core/tests/support/mod.rs"]
mod support;

use support::TestHarness;

/// Workspace marked as initialized, used by regressions that expect startup to
/// reach the prompt or provider checks rather than the initialization gate.
#[fixture]
fn initialized_harness() -> Result<TestHarness> {
    let harness = TestHarness::new().context("initialize harness workspace")?;
    let _marker = harness
        .write_file(".diapason/.keep", "")
        .context("mark workspace initialized")?;
    Ok(harness)
}

/// Build the isolated command used by every CLI failure regression.
///
/// Returns `assert_cmd::Command` rather than `std::process::Command` so a test
/// can supply stdin, which print mode reads when no inline prompt is given.
fn base_command(harness: &TestHarness) -> Result<Command> {
    let _workspace_config = harness
        .write_file(
            "config.toml",
            r#"[workspace]
use_root_config = true

[agent]
provider = "openrouter"
default_model = "xiaomi/mimo-v2.5-pro"
api_key_env = "OPENROUTER_API_KEY"
"#,
        )
        .context("write isolated workspace configuration")?;
    let _home = harness
        .write_file(".test-home/.keep", "")
        .context("create isolated test home")?;
    let _config = harness
        .write_file(".test-config/.keep", "")
        .context("create isolated test configuration directory")?;
    let _data = harness
        .write_file(".test-data/.keep", "")
        .context("create isolated test data directory")?;
    let _state = harness
        .write_file(".test-state/.keep", "")
        .context("create isolated test state directory")?;
    let _cache = harness
        .write_file(".test-cache/.keep", "")
        .context("create isolated test cache directory")?;
    let _runtime = harness
        .write_file(".test-runtime/.keep", "")
        .context("create isolated test runtime directory")?;

    let home = harness.workspace().join(".test-home");
    let config = harness.workspace().join(".test-config");
    let data = harness.workspace().join(".test-data");
    let state = harness.workspace().join(".test-state");
    let cache = harness.workspace().join(".test-cache");
    let runtime = harness.workspace().join(".test-runtime");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_diapason"));
    let _configured_command = cmd
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CACHE_HOME", cache)
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("DIAPASON_CONFIG_PATH")
        .env("NO_COLOR", "1")
        .args(["--provider", "openrouter", "--model", "xiaomi/mimo-v2.5-pro"])
        .current_dir(harness.workspace());
    Ok(cmd)
}

#[rstest::rstest]
#[case::no_input_at_all(None, None)]
#[case::whitespace_inline_prompt(Some("   "), None)]
#[case::whitespace_stdin(None, Some(" \n\t\n"))]
fn print_mode_requires_prompt_or_stdin(
    initialized_harness: Result<TestHarness>,
    #[case] inline_prompt: Option<&str>,
    #[case] stdin: Option<&str>,
) -> Result<()> {
    let harness = initialized_harness.context("prepare initialized harness")?;
    let mut cmd = base_command(&harness)?;
    let _argument = cmd.env_remove("OPENROUTER_API_KEY").arg("--print");
    if let Some(prompt) = inline_prompt {
        let _argument = cmd.arg(prompt);
    }
    if let Some(input) = stdin {
        let _argument = cmd.write_stdin(input);
    }

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("No prompt provided")
            .and(predicate::str::contains("API key not found for provider 'openrouter'").not()),
    );

    Ok(())
}

#[rstest::rstest]
#[case::inline_prompt(None)]
#[case::stdin_prompt(Some("hello\n"))]
fn print_mode_with_prompt_requires_provider_authentication(
    initialized_harness: Result<TestHarness>,
    #[case] stdin: Option<&str>,
) -> Result<()> {
    let harness = initialized_harness.context("prepare initialized harness")?;
    let mut cmd = base_command(&harness)?;
    let _argument = cmd.env_remove("OPENROUTER_API_KEY").arg("--print");
    match stdin {
        Some(input) => {
            let _argument = cmd.write_stdin(input);
        }
        None => {
            let _argument = cmd.arg("hello");
        }
    }

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("API key not found for provider 'openrouter'")
            .and(predicate::str::contains("No prompt provided").not()),
    );

    Ok(())
}

#[rstest::rstest]
fn print_mode_missing_prompt_precedes_config_failure(initialized_harness: Result<TestHarness>) -> Result<()> {
    let harness = initialized_harness.context("prepare initialized harness")?;
    let missing_config = harness.workspace().join("missing-config.toml");
    let mut cmd = base_command(&harness)?;
    let _argument = cmd
        .env_remove("OPENROUTER_API_KEY")
        .arg("--config")
        .arg(&missing_config)
        .arg("--print");

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("No prompt provided")
            .and(predicate::str::contains("API key not found for provider 'openrouter'").not())
            .and(predicate::str::contains(missing_config.to_string_lossy()).not()),
    );

    Ok(())
}

#[test]
fn config_override_failure_is_reported() -> Result<()> {
    let harness = TestHarness::new().context("initialize harness workspace")?;
    let missing_config = harness.workspace().join("missing-config.toml");

    let mut cmd = base_command(&harness)?;
    let _argument = cmd
        .arg("--workspace")
        .arg(harness.workspace())
        .arg("--config")
        .arg(&missing_config)
        .arg("--print")
        .arg("hello")
        .current_dir(harness.workspace());

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("failed to initialize Diapason startup context")
            .and(predicate::str::contains(missing_config.to_string_lossy())),
    );

    Ok(())
}

#[rstest::rstest]
fn unknown_positional_token_fails_without_forwarding_prompt_to_llm(
    initialized_harness: Result<TestHarness>,
) -> Result<()> {
    let harness = initialized_harness.context("prepare initialized harness")?;
    let mut cmd = base_command(&harness)?;
    let _argument = cmd.arg("hellp");

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("invalid value")
            .and(predicate::str::contains("is not a valid workspace path or subcommand"))
            .and(predicate::str::contains("try '--help'"))
            .and(predicate::str::contains("Sending prompt to").not()),
    );

    Ok(())
}
