#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

#[path = "../crates/codegen/vtcode-core/tests/support/mod.rs"]
mod support;

use support::TestHarness;

fn base_command(harness: &TestHarness) -> Result<Command> {
    let _workspace_config = harness
        .write_file(
            "vtcode.toml",
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
    let _vtcode_home = harness
        .write_file(".test-vtcode/.keep", "")
        .context("create isolated VT Code home")?;

    let home = harness.workspace().join(".test-home");
    let config = harness.workspace().join(".test-config");
    let data = harness.workspace().join(".test-data");
    let state = harness.workspace().join(".test-state");
    let cache = harness.workspace().join(".test-cache");
    let runtime = harness.workspace().join(".test-runtime");
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("vtcode"));
    let _configured_command = cmd
        .env_clear()
        .env("HOME", home)
        .env("VTCODE_HOME", harness.workspace().join(".test-vtcode"))
        .env("VTCODE_CONFIG", &config)
        .env("VTCODE_DATA", &data)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CACHE_HOME", cache)
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("VTCODE_CONFIG_PATH")
        .env("NO_COLOR", "1")
        .args(["--provider", "openrouter", "--model", "xiaomi/mimo-v2.5-pro"])
        .current_dir(harness.workspace());
    Ok(cmd)
}

#[test]
fn print_mode_requires_prompt_or_stdin() -> Result<()> {
    let harness = TestHarness::new().context("initialize harness workspace")?;
    let _marker = harness.write_file(".vtcode/.keep", "").context("mark workspace initialized")?;
    let mut cmd = base_command(&harness)?;
    let _argument = cmd.env_remove("OPENROUTER_API_KEY").arg("--print");

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("No prompt provided")
            .and(predicate::str::contains("API key not found for provider 'openrouter'").not()),
    );

    Ok(())
}

#[test]
fn print_mode_with_prompt_requires_provider_authentication() -> Result<()> {
    let harness = TestHarness::new().context("initialize harness workspace")?;
    let _marker = harness.write_file(".vtcode/.keep", "").context("mark workspace initialized")?;
    let mut cmd = base_command(&harness)?;
    let _argument = cmd.env_remove("OPENROUTER_API_KEY").arg("--print").arg("hello");

    let _assertion = cmd.assert().failure().stderr(
        predicate::str::contains("API key not found for provider 'openrouter'")
            .and(predicate::str::contains("No prompt provided").not()),
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
        predicate::str::contains("failed to initialize VT Code startup context")
            .and(predicate::str::contains(missing_config.to_string_lossy())),
    );

    Ok(())
}

#[test]
fn unknown_positional_token_fails_without_forwarding_prompt_to_llm() -> Result<()> {
    let harness = TestHarness::new().context("initialize harness workspace")?;
    let _marker = harness.write_file(".vtcode/.keep", "").context("mark workspace initialized")?;

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
