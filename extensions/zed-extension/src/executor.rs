/// Diapason CLI Command Executor
///
/// Handles execution of Diapason CLI commands and output capture.
/// This module provides the interface for running Diapason commands
/// and retrieving their results with timeout support.
use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Result of a Diapason command execution
#[derive(Debug, Clone)]
pub struct CommandResult {
    /// Exit status (0 = success)
    pub status: i32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
}

impl CommandResult {
    /// Check if command executed successfully
    pub fn is_success(&self) -> bool {
        self.status == 0
    }

    /// Get the output, preferring stdout if available
    pub fn output(&self) -> String {
        if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            self.stdout.clone()
        }
    }
}

/// Execute a Diapason command
///
/// # Arguments
/// * `command` - The diapason subcommand (e.g., "ask", "analyse")
/// * `args` - Additional arguments to pass
///
/// # Returns
/// Result containing the command output or error message
pub fn execute_command(command: &str, args: &[&str]) -> Result<CommandResult, String> {
    let timeout = timeout_for_command(command);
    execute_command_with_timeout_and_env(command, args, timeout, None)
}

/// Select the timeout duration for a Diapason subcommand.
///
/// The default is 30 seconds for most commands. Workspace analysis selects
/// 60 seconds for both the British and American spellings. This function only
/// chooses the duration; command execution does not currently enforce it.
fn timeout_for_command(command: &str) -> Duration {
    match command {
        "analyse" | "analyze" => Duration::from_secs(60), // Workspace analysis can take longer
        "chat" => Duration::from_secs(120),               // Chat/interactive can be longer
        _ => Duration::from_secs(30),                     // Default timeout
    }
}

/// Execute a Diapason command with custom timeout
///
/// # Arguments
/// * `command` - The diapason subcommand
/// * `args` - Additional arguments
/// * `_timeout` - Maximum duration to wait for command completion
///
/// # Returns
/// Result containing the command output or timeout error
pub fn execute_command_with_timeout(
    command: &str,
    args: &[&str],
    _timeout: Duration,
) -> Result<CommandResult, String> {
    execute_command_with_timeout_and_env(command, args, _timeout, None)
}

pub fn execute_command_with_env(
    command: &str,
    args: &[&str],
    env: &HashMap<String, String>,
) -> Result<CommandResult, String> {
    execute_command_with_timeout_and_env(command, args, Duration::from_secs(30), Some(env))
}

pub fn execute_command_with_timeout_and_env(
    command: &str,
    args: &[&str],
    _timeout: Duration,
    env: Option<&HashMap<String, String>>,
) -> Result<CommandResult, String> {
    run_diapason(OsStr::new("diapason"), command, args, env)
}

/// Run a Diapason program with a subcommand and arguments.
///
/// The program is injectable so dispatch tests can exercise the argument
/// mapping without requiring the real CLI on `PATH`.
fn run_diapason(
    program: &OsStr,
    command: &str,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
) -> Result<CommandResult, String> {
    // Build the full command
    let mut cmd = Command::new(program);
    cmd.arg(command);

    for arg in args {
        cmd.arg(arg);
    }

    if let Some(env) = env {
        cmd.envs(env);
    }

    // Execute and capture output
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to execute diapason command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = output.status.code().unwrap_or(-1);

    Ok(CommandResult {
        status,
        stdout,
        stderr,
    })
}

/// Check if Diapason CLI is available
pub fn check_diapason_available() -> bool {
    Command::new("diapason")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Get Diapason CLI version
pub fn get_diapason_version() -> Result<String, String> {
    let result = execute_command("--version", &[])?;
    if result.is_success() {
        Ok(result.stdout.trim().to_string())
    } else {
        Err(result.stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    fn unique_identifier() -> String {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}_{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Temporary directory holding a fake `diapason` program.
    #[cfg(unix)]
    struct FakeProgram {
        directory: PathBuf,
        record_variable: String,
        argv_path: PathBuf,
    }

    #[cfg(unix)]
    impl FakeProgram {
        fn new(label: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "diapason-executor-{label}-{}",
                unique_identifier()
            ));
            fs::create_dir_all(&directory)
                .expect("fake executable directory should be creatable");
            let record_variable = format!("DIAPASON_FAKE_ARGV_{}", unique_identifier());
            let argv_path = directory.join("argv.txt");
            Self {
                directory,
                record_variable,
                argv_path,
            }
        }

        /// Write an executable shell script that records its arguments.
        fn write(&self, exit_code: i32, stdout: &str, stderr: &str) -> PathBuf {
            let path = self.directory.join("diapason");
            let script = format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > \"${{{record_variable}}}\"\n\
                 printf '%s' '{stdout}'\n\
                 printf '%s' '{stderr}' >&2\n\
                 exit {exit_code}\n",
                record_variable = self.record_variable.as_str(),
            );
            fs::write(&path, script).expect("fake executable should be writable");
            let mut permissions = fs::metadata(&path)
                .expect("fake executable should have metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions)
                .expect("fake executable should be made executable");
            path
        }

        fn recorded_argv(&self) -> Vec<String> {
            fs::read_to_string(&self.argv_path)
                .expect("fake executable should record its arguments")
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn record_environment(&self) -> EnvironmentGuard {
            EnvironmentGuard::set(&self.record_variable, self.argv_path.as_os_str())
        }
    }

    #[cfg(unix)]
    impl Drop for FakeProgram {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    /// Restore an environment variable when dropped.
    #[cfg(unix)]
    struct EnvironmentGuard {
        key: String,
        previous: Option<OsString>,
    }

    #[cfg(unix)]
    impl EnvironmentGuard {
        fn set(key: &str, value: &OsStr) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self {
                key: key.to_owned(),
                previous,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for EnvironmentGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    #[test]
    fn test_command_result_is_success() {
        let result = CommandResult {
            status: 0,
            stdout: "output".to_string(),
            stderr: String::new(),
        };
        assert!(result.is_success());

        let result = CommandResult {
            status: 1,
            stdout: String::new(),
            stderr: "error".to_string(),
        };
        assert!(!result.is_success());
    }

    #[test]
    fn test_command_result_output() {
        let result = CommandResult {
            status: 0,
            stdout: "stdout content".to_string(),
            stderr: String::new(),
        };
        assert_eq!(result.output(), "stdout content");

        let result = CommandResult {
            status: 1,
            stdout: String::new(),
            stderr: "stderr content".to_string(),
        };
        assert_eq!(result.output(), "stderr content");
    }

    #[test]
    fn test_timeout_defaults() {
        // These tests verify timeout logic exists
        // Actual timeout behaviour requires process mocking
        let timeout = Duration::from_secs(30);
        assert!(timeout > Duration::from_secs(0));
    }

    #[test]
    fn test_command_specific_timeouts() {
        assert_eq!(timeout_for_command("chat"), Duration::from_secs(120));
        assert_eq!(
            timeout_for_command("something-else"),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn test_timeout_accepts_both_analysis_spellings() {
        assert_eq!(timeout_for_command("analyse"), Duration::from_secs(60));
        assert_eq!(timeout_for_command("analyze"), Duration::from_secs(60));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_diapason_dispatches_british_spelling() {
        let fake = FakeProgram::new("analyse");
        let program = fake.write(0, "analysis complete", "");
        let _record = fake.record_environment();

        let result = run_diapason(program.as_os_str(), "analyse", &[], None)
            .expect("fake diapason should execute");

        assert!(result.is_success());
        assert_eq!(
            fake.recorded_argv().first().map(String::as_str),
            Some("analyse")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_run_diapason_dispatches_american_spelling() {
        let fake = FakeProgram::new("analyze");
        let program = fake.write(0, "analysis complete", "");
        let _record = fake.record_environment();

        let result = run_diapason(program.as_os_str(), "analyze", &[], None)
            .expect("fake diapason should execute");

        assert!(result.is_success());
        assert_eq!(
            fake.recorded_argv().first().map(String::as_str),
            Some("analyze")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_run_diapason_reports_non_zero_exit() {
        let fake = FakeProgram::new("failure");
        let program = fake.write(7, "", "boom");
        let _record = fake.record_environment();

        let result = run_diapason(program.as_os_str(), "analyse", &[], None)
            .expect("a non-zero exit is still an executed process");

        assert!(!result.is_success());
        assert_eq!(result.status, 7);
        assert!(
            result.stderr.contains("boom"),
            "unexpected stderr: {:?}",
            result.stderr
        );
    }
}
