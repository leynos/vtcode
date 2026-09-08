//! Test-only Git fixtures and clean-child execution support.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{NamedTempFile, TempDir};
use vtcode_commons::canonicalize;

const FIXTURE_COMMIT_MESSAGE: &str = "git-info fixture commit";
pub(crate) const CLEAN_TEST_CHILD_ENV: &str = "VTCODE_GIT_INFO_CLEAN_TEST_CHILD";
pub(crate) const FIXTURE_REMOTE_URL: &str = "https://example.invalid/vtcode.git";
const FIXTURE_AUTHOR_NAME: &str = "VTCode Test";
const FIXTURE_AUTHOR_EMAIL: &str = "vtcode-test@example.invalid";
const FIXTURE_COMMIT_DATE: &str = "2000-01-02T03:04:05+00:00";
const GIT_ENV_OVERRIDES: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG",
];
const CHILD_OUTPUT_LIMIT: usize = 4096;
// This bounds a hung libtest child without making normal test duration an
// assertion. The child is polled infrequently enough to avoid busy-waiting
// on a loaded CI worker.
const CLEAN_TEST_CHILD_WATCHDOG: Duration = Duration::from_secs(60);
const CLEAN_TEST_CHILD_POLL: Duration = Duration::from_millis(50);

pub(crate) struct GitFixture {
    _temp_dir: TempDir,
    pub(crate) repo_root: PathBuf,
    pub(crate) short_head: String,
}

fn fixture_git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", cwd.join("fixture-global-git-config"))
        .env("GIT_TEMPLATE_DIR", cwd)
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", FIXTURE_AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", FIXTURE_AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", FIXTURE_AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", FIXTURE_AUTHOR_EMAIL)
        .env("GIT_AUTHOR_DATE", FIXTURE_COMMIT_DATE)
        .env("GIT_COMMITTER_DATE", FIXTURE_COMMIT_DATE);
    command
}

fn bounded_child_capture(file: &mut NamedTempFile) -> Result<String> {
    let file_size = file.as_file().metadata().context("inspect child output capture")?.len();
    let mut bytes = Vec::with_capacity(file_size.min(CHILD_OUTPUT_LIMIT as u64) as usize);
    file.as_file_mut()
        .seek(SeekFrom::Start(0))
        .context("rewind child output capture")?;
    file.as_file_mut()
        .take(CHILD_OUTPUT_LIMIT as u64)
        .read_to_end(&mut bytes)
        .context("read child output capture")?;
    let output = String::from_utf8_lossy(&bytes);
    if file_size > CHILD_OUTPUT_LIMIT as u64 {
        Ok(format!("{output} [truncated]"))
    } else {
        Ok(output.into_owned())
    }
}

fn describe_captured_child_failure(status: &ExitStatus, stdout: &str, stderr: &str) -> String {
    format!("status={status} stdout={stdout:?} stderr={stderr:?}")
}

#[cfg(unix)]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // This targets the group created by `CommandExt::process_group(0)`.
    vtcode_bash_runner::kill_process_group_by_pid(child.id())
}

#[cfg(windows)]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // The existing Windows helper uses `taskkill /T /F` for the child tree.
    vtcode_bash_runner::kill_process(child.id())
}

#[cfg(not(any(unix, windows)))]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // Other targets have no process-tree helper; the direct kill below is the
    // cleanup fallback.
    let _ = child;
    Ok(())
}

fn reap_after_child_error(
    child: &mut Child,
    reason: &str,
    stdout_capture: &mut NamedTempFile,
    stderr_capture: &mut NamedTempFile,
) -> Result<()> {
    let process_group_result = kill_child_process_group(child);
    let kill_result = child.kill();
    let wait_result = child.wait();
    let stdout = bounded_child_capture(stdout_capture)?;
    let stderr = bounded_child_capture(stderr_capture)?;
    bail!(
        "clean Git test child {reason}; process_group_kill={process_group_result:?} kill={kill_result:?} wait={wait_result:?} stdout={stdout:?} stderr={stderr:?}"
    )
}

fn verify_clean_test_environment(test_name: &str) -> Result<()> {
    for variable in GIT_ENV_OVERRIDES {
        if std::env::var_os(variable).is_some() {
            bail!("clean Git test child {test_name:?} received inherited override {variable:?}");
        }
    }
    Ok(())
}

pub(crate) fn no_command_configuration(_: &mut Command) {}

/// Run a test body in one child with the Git environment overrides removed.
///
/// `configure` is called only by the parent, before the eight Git overrides
/// are removed and the exact-name sentinel is set. The sentineled child
/// verifies the removals before invoking `body`.
pub(crate) fn run_in_clean_test_process<F>(test_name: &str, configure: F, body: fn() -> Result<()>) -> Result<()>
where
    F: FnOnce(&mut Command),
{
    match std::env::var_os(CLEAN_TEST_CHILD_ENV) {
        Some(value) if value == OsStr::new(test_name) => {
            verify_clean_test_environment(test_name)?;
            body()
        }
        Some(_) => bail!("clean Git test child sentinel does not match the requested test name"),
        None => {
            let current_exe = std::env::current_exe().context("resolve current libtest executable")?;
            let mut stdout_capture = NamedTempFile::new().context("create child stdout capture")?;
            let mut stderr_capture = NamedTempFile::new().context("create child stderr capture")?;
            let mut command = Command::new(current_exe);
            command.args([test_name, "--exact", "--nocapture"]);
            configure(&mut command);
            for variable in GIT_ENV_OVERRIDES {
                command.env_remove(variable);
            }
            command.env(CLEAN_TEST_CHILD_ENV, test_name);
            command
                .stdout(Stdio::from(stdout_capture.reopen().context("open child stdout capture")?))
                .stderr(Stdio::from(stderr_capture.reopen().context("open child stderr capture")?));
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;

                command.process_group(0);
            }
            let mut child = command.spawn().context("spawn clean Git test child")?;
            let deadline = Instant::now() + CLEAN_TEST_CHILD_WATCHDOG;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) if Instant::now() < deadline => thread::sleep(CLEAN_TEST_CHILD_POLL),
                    Ok(None) => {
                        return reap_after_child_error(
                            &mut child,
                            &format!("watchdog expired after {:?} for test {test_name:?}", CLEAN_TEST_CHILD_WATCHDOG),
                            &mut stdout_capture,
                            &mut stderr_capture,
                        );
                    }
                    Err(error) => {
                        return reap_after_child_error(
                            &mut child,
                            &format!("polling failed for test {test_name:?}: {error}"),
                            &mut stdout_capture,
                            &mut stderr_capture,
                        );
                    }
                }
            };
            if !status.success() {
                let stdout = bounded_child_capture(&mut stdout_capture)?;
                let stderr = bounded_child_capture(&mut stderr_capture)?;
                bail!("clean Git test child failed: {}", describe_captured_child_failure(&status, &stdout, &stderr));
            }
            Ok(())
        }
    }
}

fn run_fixture_git(cwd: &Path, args: &[&str]) -> Result<Output> {
    let output = fixture_git_command(cwd)
        .args(args)
        .output()
        .with_context(|| format!("run fixture git command: git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "fixture git command failed (git {}): {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

pub(crate) fn isolated_git_repository() -> Result<GitFixture> {
    isolated_git_repository_with(FIXTURE_REMOTE_URL, FIXTURE_COMMIT_MESSAGE)
}

pub(crate) fn isolated_git_repository_with(remote_url: &str, commit_message: &str) -> Result<GitFixture> {
    let temp_dir = TempDir::new().context("create isolated Git fixture directory")?;
    let repo_root = temp_dir.path().join("repository");
    run_fixture_git(
        temp_dir.path(),
        &[
            "init",
            "--quiet",
            "--initial-branch=main",
            "--object-format=sha1",
            "repository",
        ],
    )?;
    let repo_root = canonicalize(repo_root).context("canonicalize isolated Git fixture root")?;
    run_fixture_git(&repo_root, &["config", "core.abbrev", "7"])?;
    run_fixture_git(&repo_root, &["remote", "add", "origin", remote_url])?;
    run_fixture_git(&repo_root, &["commit", "--quiet", "--allow-empty", "--message", commit_message])?;
    let head_output = run_fixture_git(&repo_root, &["rev-parse", "--short", "HEAD"])?;
    let short_head = String::from_utf8(head_output.stdout)
        .context("decode fixture Git short HEAD")?
        .trim()
        .to_owned();
    if short_head.is_empty() {
        bail!("fixture Git repository must have a short HEAD");
    }

    Ok(GitFixture { _temp_dir: temp_dir, repo_root, short_head })
}
