#![allow(
    missing_docs,
    clippy::expect_used,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
#![cfg(unix)]
use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_fake_diapason(temp_dir: &TempDir) -> PathBuf {
    let fake_diapason = temp_dir.path().join("diapason");
    let body = format!(
        "#!/bin/sh\n\
printf '%s\\n' \"$@\" > '{args_log}'\n\
pwd > '{cwd_log}'\n",
        args_log = temp_dir.path().join("diapason-args.log").display(),
        cwd_log = temp_dir.path().join("diapason-cwd.log").display(),
    );
    fs::write(&fake_diapason, body).expect("write fake diapason");

    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(&fake_diapason).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_diapason, permissions).expect("chmod");

    fake_diapason
}

#[test]
fn check_script_delegates_ast_grep_to_diapason_command() {
    let fake_bin = TempDir::new().expect("fake bin");
    let _fake_diapason = create_fake_diapason(&fake_bin);
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let mut command = Command::new("bash");
    let _configured_command = command
        .current_dir(&workspace)
        .arg("scripts/check.sh")
        .arg("ast-grep")
        .env("PATH", current_path)
        .env("DIAPASON_CHECK_SCRIPT_DIAPASON_BIN", fake_bin.path().join("diapason"));

    let _assertion = command.assert().success();

    let args = fs::read_to_string(fake_bin.path().join("diapason-args.log")).expect("read args log");
    assert_eq!(args.lines().collect::<Vec<_>>(), ["check", "ast-grep"]);

    let cwd = fs::read_to_string(fake_bin.path().join("diapason-cwd.log")).expect("read cwd log");
    // `getcwd` may report a different path case than `CARGO_MANIFEST_DIR` on a
    // case-insensitive filesystem (e.g. macOS APFS), so compare case-insensitively.
    assert_eq!(cwd.trim().to_lowercase(), workspace.display().to_string().to_lowercase());
}
