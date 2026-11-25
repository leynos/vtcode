//! Test-only Git fixtures and clean-child execution support.

use anyhow::{Context, Result, bail};
use diapason_commons::canonicalize;
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{NamedTempFile, TempDir};

const FIXTURE_COMMIT_MESSAGE: &str = "git-info fixture commit";
pub(crate) const CLEAN_TEST_CHILD_ENV: &str = "DIAPASON_GIT_INFO_CLEAN_TEST_CHILD";
pub(crate) const FIXTURE_REMOTE_URL: &str = "https://example.invalid/diapason.git";
const FIXTURE_AUTHOR_NAME: &str = "Diapason Test";
const FIXTURE_AUTHOR_EMAIL: &str = "diapason-test@example.invalid";
const FIXTURE_COMMIT_DATE: &str = "2000-01-02T03:04:05+00:00";
/// Git environment variables that can redirect a command away from the
/// repository, object store, or configuration a fixture intends to use.
///
/// The list mirrors the local isolation scrub in `scripts/test-git-cliff.sh`
/// and extends it with `GIT_NAMESPACE` and `GIT_CEILING_DIRECTORIES`. Keep it
/// sorted so a new Git redirect knob has an obvious home, and remove every
/// entry from both fixture commands and the sentineled clean child.
const GIT_ENV_OVERRIDES: [&str; 25] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_EXEC_PATH",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_NAMESPACE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_TEMPLATE_DIR",
    "GIT_WORK_TREE",
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
    command.current_dir(cwd);
    // The fixture pins its own Git configuration below, so every inherited
    // redirect variable is removed rather than overridden.
    for variable in GIT_ENV_OVERRIDES {
        command.env_remove(variable);
    }
    command
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

/// Read the bounded tail of a child output capture.
///
/// libtest writes its summary after any test output, so the tail is where the
/// `N passed` line lives even when a verbose child fills the head of the file.
fn bounded_child_tail_capture(file: &mut NamedTempFile) -> Result<String> {
    let file_size = file.as_file().metadata().context("inspect child output capture")?.len();
    let limit = CHILD_OUTPUT_LIMIT as u64;
    let offset = file_size.saturating_sub(limit);
    let mut bytes = Vec::with_capacity((file_size - offset) as usize);
    file.as_file_mut()
        .seek(SeekFrom::Start(offset))
        .context("seek child output capture tail")?;
    file.as_file_mut()
        .take(limit)
        .read_to_end(&mut bytes)
        .context("read child output capture tail")?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn describe_captured_child_failure(status: &ExitStatus, stdout: &str, stderr: &str) -> String {
    format!("status={status} stdout={stdout:?} stderr={stderr:?}")
}

#[cfg(unix)]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // This targets the group created by `CommandExt::process_group(0)`.
    diapason_bash_runner::kill_process_group_by_pid(child.id())
}

#[cfg(windows)]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // The existing Windows helper uses `taskkill /T /F` for the child tree.
    diapason_bash_runner::kill_process(child.id())
}

#[cfg(not(any(unix, windows)))]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // Other targets have no process-tree helper; the direct kill below is the
    // cleanup fallback.
    let _ = child;
    Ok(())
}

fn reap_after_child_error<T>(
    child: &mut Child,
    reason: &str,
    stdout_capture: &mut NamedTempFile,
    stderr_capture: &mut NamedTempFile,
) -> Result<T> {
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

/// Report whether this process is the sentineled child for `test_name`.
///
/// A sentinel naming a different test is a harness error: the inherited
/// environment did not belong to the requested test.
fn is_child_sentinel(test_name: &str) -> Result<bool> {
    match std::env::var_os(CLEAN_TEST_CHILD_ENV) {
        Some(value) if value == OsStr::new(test_name) => Ok(true),
        Some(_) => bail!("clean Git test child sentinel does not match the requested test name"),
        None => Ok(false),
    }
}

/// Child stdout and stderr captures kept open for reporting after the wait.
struct CleanTestChildCapture {
    stdout: NamedTempFile,
    stderr: NamedTempFile,
}

impl CleanTestChildCapture {
    fn create() -> Result<Self> {
        Ok(Self {
            stdout: NamedTempFile::new().context("create child stdout capture")?,
            stderr: NamedTempFile::new().context("create child stderr capture")?,
        })
    }
}

/// Build the libtest invocation for the sentineled child.
///
/// `configure` runs first, so parent-supplied values such as adversarial
/// `GIT_DIR` and `GIT_WORK_TREE` are still scrubbed with the rest of the
/// overrides before the child sentinel is set.
fn build_child_command<F>(
    current_exe: PathBuf,
    test_name: &str,
    configure: F,
    captures: &CleanTestChildCapture,
) -> Result<Command>
where
    F: FnOnce(&mut Command),
{
    let mut command = Command::new(current_exe);
    command.args([test_name, "--exact", "--nocapture"]);
    configure(&mut command);
    for variable in GIT_ENV_OVERRIDES {
        command.env_remove(variable);
    }
    command.env(CLEAN_TEST_CHILD_ENV, test_name);
    command
        .stdout(Stdio::from(captures.stdout.reopen().context("open child stdout capture")?))
        .stderr(Stdio::from(captures.stderr.reopen().context("open child stderr capture")?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }
    Ok(command)
}

/// Spawn the child and wait for it under the watchdog.
fn spawn_child_with_watchdog(
    mut command: Command,
    captures: &mut CleanTestChildCapture,
    test_name: &str,
) -> Result<ExitStatus> {
    let mut child = command.spawn().context("spawn clean Git test child")?;
    let deadline = Instant::now() + CLEAN_TEST_CHILD_WATCHDOG;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(CLEAN_TEST_CHILD_POLL),
            Ok(None) => {
                return reap_after_child_error(
                    &mut child,
                    &format!("watchdog expired after {:?} for test {test_name:?}", CLEAN_TEST_CHILD_WATCHDOG),
                    &mut captures.stdout,
                    &mut captures.stderr,
                );
            }
            Err(error) => {
                return reap_after_child_error(
                    &mut child,
                    &format!("polling failed for test {test_name:?}: {error}"),
                    &mut captures.stdout,
                    &mut captures.stderr,
                );
            }
        }
    }
}

/// Number of tests libtest reported as passed in its final summary.
///
/// libtest prints `running N tests` before the run and
/// `test result: ok. N passed; …` afterwards. The result line is preferred
/// because it is the authoritative outcome; the running line is a fallback
/// for a child that exited before printing a summary.
fn reported_passed_test_count(stdout: &str) -> Option<usize> {
    stdout
        .lines()
        .rev()
        .find_map(|line| {
            let summary = line.trim().strip_prefix("test result:")?;
            let passed = summary.split(';').next()?.trim().split_once(" passed")?.0;
            passed.rsplit('.').next()?.trim().parse().ok()
        })
        .or_else(|| {
            stdout.lines().find_map(|line| {
                let count = line.trim().strip_prefix("running ")?;
                let count = count.strip_suffix(" tests").or_else(|| count.strip_suffix(" test"))?;
                count.trim().parse().ok()
            })
        })
}

pub(crate) fn no_command_configuration(_: &mut Command) {}

/// Run a test body in one child with the Git environment overrides removed.
///
/// `configure` is called only by the parent, before the Git overrides are
/// removed and the exact-name sentinel is set. The sentineled child verifies
/// the removals before invoking `body`, and the parent then confirms libtest
/// actually ran the requested test so a drifted name cannot pass silently.
pub(crate) fn run_in_clean_test_process<F>(test_name: &str, configure: F, body: fn() -> Result<()>) -> Result<()>
where
    F: FnOnce(&mut Command),
{
    if is_child_sentinel(test_name)? {
        verify_clean_test_environment(test_name)?;
        return body();
    }

    let current_exe = std::env::current_exe().context("resolve current libtest executable")?;
    let mut captures = CleanTestChildCapture::create()?;
    let command = build_child_command(current_exe, test_name, configure, &captures)?;
    let status = spawn_child_with_watchdog(command, &mut captures, test_name)?;
    if !status.success() {
        let stdout = bounded_child_capture(&mut captures.stdout)?;
        let stderr = bounded_child_capture(&mut captures.stderr)?;
        bail!("clean Git test child failed: {}", describe_captured_child_failure(&status, &stdout, &stderr));
    }

    // The child runs with `--exact`, so a renamed or removed test makes
    // libtest execute zero tests and still exit successfully.
    let stdout = bounded_child_tail_capture(&mut captures.stdout)?;
    match reported_passed_test_count(&stdout) {
        Some(0) => bail!(
            "clean Git test child {test_name:?} ran zero tests; the exact filter no longer matches a test: stdout={stdout:?}"
        ),
        Some(_) => Ok(()),
        None => bail!("clean Git test child {test_name:?} did not report a libtest summary: stdout={stdout:?}"),
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
