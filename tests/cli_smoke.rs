#![allow(
    missing_docs,
    clippy::expect_used,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn isolated_diapason_command() -> (TempDir, Command) {
    let home = tempfile::tempdir().expect("create temp home");
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("diapason");
    let _configured_command = cmd
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_STATE_HOME", home.path().join(".state"))
        .env("XDG_DATA_HOME", home.path().join(".data"))
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env_remove("DIAPASON_CONFIG_PATH")
        .current_dir(home.path())
        .env("NO_COLOR", "1");
    (home, cmd)
}

#[test]
fn diapason_help_command_succeeds() {
    let (_home, mut cmd) = isolated_diapason_command();
    let _argument = cmd.arg("--help");
    let _assertion = cmd.assert().success();
}

#[test]
fn diapason_tool_policy_status_succeeds() {
    let (_home, mut cmd) = isolated_diapason_command();
    let _argument = cmd.arg("tool-policy").arg("status");
    let _assertion = cmd.assert().success();
}

#[test]
fn diapason_tool_policy_help_succeeds() {
    let (_home, mut cmd) = isolated_diapason_command();
    let _argument = cmd.arg("tool-policy").arg("--help");
    let _assertion = cmd.assert().success();
}

#[test]
fn diapason_positional_workspace_with_tool_policy_status_succeeds() {
    let (home, mut cmd) = isolated_diapason_command();
    let workspace = home.path().join("workspace");
    std::fs::create_dir(&workspace).expect("create workspace");

    let _argument = cmd.arg("workspace").arg("tool-policy").arg("status");
    let _assertion = cmd.assert().success();
}

#[test]
fn diapason_invalid_option_shows_usage_help() {
    let (_home, mut cmd) = isolated_diapason_command();
    let _argument = cmd.arg("--definitely-invalid-option");
    let _assertion = cmd.assert().failure().stderr(contains("Usage: diapason"));
}
