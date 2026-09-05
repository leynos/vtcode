use anyhow::{Context, Result, anyhow, bail};
use crossterm::style::Stylize;
use std::path::Path;
use tokio::process::Command;
use vtcode_core::cli::args::CheckSubcommand;
use vtcode_core::tools::ast_grep_binary::{missing_ast_grep_message, resolve_ast_grep_binary_from_env_and_fs};

const AST_GREP_CONFIG_PATH: &str = "sgconfig.yml";
const AST_GREP_INIT_COMMAND: &str = "vtcode init";

pub async fn handle_check_command(workspace: &Path, command: CheckSubcommand) -> Result<()> {
    match command {
        CheckSubcommand::AstGrep => handle_ast_grep_check(workspace).await,
    }
}

async fn handle_ast_grep_check(workspace: &Path) -> Result<()> {
    let ast_grep = resolve_ast_grep_binary_from_env_and_fs().ok_or_else(|| {
        anyhow!(missing_ast_grep_message(
            "After installation, run `vtcode init` to materialize the local ast-grep scaffold."
        ))
    })?;

    let config_path = workspace.join(AST_GREP_CONFIG_PATH);
    if !config_path.is_file() {
        bail!(
            "ast-grep scaffold is missing in {}. Run `{AST_GREP_INIT_COMMAND}` to materialize `sgconfig.yml`, `rules/`, and `rule-tests/` for this workspace.",
            workspace.display()
        );
    }

    println!("{}", "→ Running ast-grep rule tests...".cyan());
    run_ast_grep_subcommand(workspace, &ast_grep, "test")
        .await
        .with_context(|| "ast-grep rule tests failed")?;

    println!("{}", "→ Running ast-grep repository scan...".cyan());
    run_ast_grep_subcommand(workspace, &ast_grep, "scan")
        .await
        .with_context(|| "ast-grep scan found issues")?;

    println!("{}", "✓ ast-grep rules passed!".green());
    Ok(())
}

async fn run_ast_grep_subcommand(workspace: &Path, ast_grep: &Path, subcommand: &str) -> Result<()> {
    let status = Command::new(ast_grep)
        .current_dir(workspace)
        .arg(subcommand)
        .arg("--config")
        .arg(AST_GREP_CONFIG_PATH)
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .status()
        .await
        .with_context(|| format!("failed to run ast-grep {subcommand}"))?;

    if status.success() {
        return Ok(());
    }

    if subcommand == "test" {
        bail!("ast-grep exited with status {status} while running tests");
    }

    bail!("ast-grep exited with status {status} while running scan");
}

#[cfg(test)]
mod tests {
    use super::{AST_GREP_CONFIG_PATH, CheckSubcommand, handle_check_command};
    use anyhow::{Context, Result, bail, ensure};
    use serial_test::serial;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use vtcode_commons::canonicalize;
    use vtcode_core::tools::ast_grep_binary::{AST_GREP_INSTALL_COMMAND, set_ast_grep_binary_override_for_tests};

    fn create_workspace_with_scaffold() -> Result<TempDir> {
        let temp_dir = TempDir::new().context("create temporary ast-grep workspace")?;
        fs::write(temp_dir.path().join(AST_GREP_CONFIG_PATH), "ruleDirs: []\n")
            .context("write ast-grep scaffold config")?;
        Ok(temp_dir)
    }

    fn create_ast_grep_stub(temp_dir: &TempDir, test_exit_code: i32, scan_exit_code: i32) -> Result<PathBuf> {
        #[cfg(windows)]
        let script_path = temp_dir.path().join("ast-grep-stub.cmd");
        #[cfg(not(windows))]
        let script_path = temp_dir.path().join("ast-grep-stub.sh");

        let log_dir = temp_dir.path().display().to_string();

        #[cfg(windows)]
        let body = format!(
            "@echo off\r\n\
set subcommand=%~1\r\n\
set log_dir={log_dir}\r\n\
> \"%log_dir%\\%subcommand%-args.log\" echo %subcommand%\r\n\
shift\r\n\
:args\r\n\
if \"%~1\"==\"\" goto done_args\r\n\
>> \"%log_dir%\\%subcommand%-args.log\" echo %~1\r\n\
shift\r\n\
goto args\r\n\
:done_args\r\n\
> \"%log_dir%\\%subcommand%-cwd.log\" echo %CD%\r\n\
if /I \"%subcommand%\"==\"test\" exit /b {test_exit_code}\r\n\
if /I \"%subcommand%\"==\"scan\" exit /b {scan_exit_code}\r\n\
exit /b 0\r\n"
        );

        #[cfg(not(windows))]
        let body = format!(
            "#!/bin/sh\n\
subcommand=\"$1\"\n\
shift\n\
log_dir='{log_dir}'\n\
{{\n\
  printf '%s\\n' \"$subcommand\"\n\
  for arg in \"$@\"; do\n\
    printf '%s\\n' \"$arg\"\n\
  done\n\
}} > \"$log_dir/$subcommand-args.log\"\n\
pwd > \"$log_dir/$subcommand-cwd.log\"\n\
case \"$subcommand\" in\n\
  test) exit {test_exit_code} ;;\n\
  scan) exit {scan_exit_code} ;;\n\
esac\n\
exit 0\n"
        );

        fs::write(&script_path, body).with_context(|| format!("write ast-grep stub at {}", script_path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&script_path)
                .with_context(|| format!("read ast-grep stub metadata at {}", script_path.display()))?
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions)
                .with_context(|| format!("make ast-grep stub executable at {}", script_path.display()))?;
        }

        Ok(script_path)
    }

    fn read_lines(path: &Path) -> Result<Vec<String>> {
        Ok(fs::read_to_string(path)
            .with_context(|| format!("read ast-grep log at {}", path.display()))?
            .lines()
            .map(ToString::to_string)
            .collect())
    }

    #[tokio::test]
    #[serial]
    async fn ast_grep_check_runs_test_then_scan_from_workspace_root() -> Result<()> {
        let workspace = create_workspace_with_scaffold()?;
        let stub = create_ast_grep_stub(&workspace, 0, 0)?;
        let _guard = set_ast_grep_binary_override_for_tests(Some(stub));

        handle_check_command(workspace.path(), CheckSubcommand::AstGrep)
            .await
            .context("ast-grep check should pass")?;

        let test_args = read_lines(&workspace.path().join("test-args.log"))?;
        ensure!(
            test_args == ["test", "--config", AST_GREP_CONFIG_PATH],
            "unexpected ast-grep test arguments: {test_args:?}"
        );

        let scan_args = read_lines(&workspace.path().join("scan-args.log"))?;
        ensure!(
            scan_args == ["scan", "--config", AST_GREP_CONFIG_PATH],
            "unexpected ast-grep scan arguments: {scan_args:?}"
        );

        let expected_workspace = canonicalize(workspace.path()).context("canonicalize test workspace")?;

        let test_cwd = fs::read_to_string(workspace.path().join("test-cwd.log"))
            .context("read ast-grep test working directory")?;
        let test_cwd = canonicalize(test_cwd.trim()).context("canonicalize ast-grep test working directory")?;
        ensure!(
            test_cwd == expected_workspace,
            "ast-grep test ran from {test_cwd:?}, expected {expected_workspace:?}"
        );

        let scan_cwd = fs::read_to_string(workspace.path().join("scan-cwd.log"))
            .context("read ast-grep scan working directory")?;
        let scan_cwd = canonicalize(scan_cwd.trim()).context("canonicalize ast-grep scan working directory")?;
        ensure!(
            scan_cwd == expected_workspace,
            "ast-grep scan ran from {scan_cwd:?}, expected {expected_workspace:?}"
        );

        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn ast_grep_check_reports_missing_binary() -> Result<()> {
        let workspace = create_workspace_with_scaffold()?;
        let _guard = set_ast_grep_binary_override_for_tests(None);

        let result = handle_check_command(workspace.path(), CheckSubcommand::AstGrep).await;

        let error = match result {
            Err(error) => error.to_string(),
            Ok(()) => bail!("missing binary check unexpectedly succeeded"),
        };
        ensure!(
            error.contains(AST_GREP_INSTALL_COMMAND),
            "missing binary error did not mention installation: {error}"
        );

        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn ast_grep_check_reports_missing_scaffold() -> Result<()> {
        let workspace = TempDir::new().context("create temporary workspace without scaffold")?;
        let stub = create_ast_grep_stub(&workspace, 0, 0)?;
        let _guard = set_ast_grep_binary_override_for_tests(Some(stub));

        let error = match handle_check_command(workspace.path(), CheckSubcommand::AstGrep).await {
            Err(error) => error.to_string(),
            Ok(()) => bail!("missing scaffold check unexpectedly succeeded"),
        };

        ensure!(error.contains("vtcode init"), "missing scaffold error did not mention vtcode init: {error}");

        Ok(())
    }
}
