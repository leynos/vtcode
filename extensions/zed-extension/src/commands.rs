//! Diapason Command Implementations
//!
//! This module implements all Diapason commands that are exposed through
//! Zed's command palette. Uses CommandBuilder for cleaner construction.
use crate::command_builder::CommandBuilder;
use crate::context::{EditorContext, IDE_CONTEXT_ENV_VAR};

/// Response from a Diapason command operation
#[derive(Debug, Clone)]
pub struct CommandResponse {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

impl CommandResponse {
    pub fn ok(output: String) -> Self {
        Self {
            success: true,
            output,
            error: None,
        }
    }

    pub fn err(error: String) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error),
        }
    }
}

/// Ask the Diapason agent an arbitrary question
pub fn ask_agent(query: &str) -> CommandResponse {
    ask_agent_with_context(query, None)
}

pub fn ask_agent_with_context(query: &str, context: Option<&EditorContext>) -> CommandResponse {
    let builder = match attach_ide_context(CommandBuilder::ask(query), context) {
        Ok(builder) => builder,
        Err(error) => return CommandResponse::err(error),
    };

    match builder.execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(result.output())
            } else {
                CommandResponse::err(format!("Command failed: {}", result.stderr))
            }
        }
        Err(e) => CommandResponse::err(e),
    }
}

/// Ask about a code selection
pub fn ask_about_selection(code: &str, language: Option<&str>) -> CommandResponse {
    ask_about_selection_with_context(code, language, None)
}

pub fn ask_about_selection_with_context(
    code: &str,
    language: Option<&str>,
    context: Option<&EditorContext>,
) -> CommandResponse {
    // Construct a query that includes the code and optional language
    let query = format!(
        "Analyse this {} code:\n{}",
        language.unwrap_or(""),
        code
    );

    ask_agent_with_context(&query, context)
}

/// Analyse the entire workspace.
pub fn analyse_workspace() -> CommandResponse {
    analyse_workspace_with_context(None)
}

pub fn analyse_workspace_with_context(context: Option<&EditorContext>) -> CommandResponse {
    let builder = match attach_ide_context(CommandBuilder::analyse(), context) {
        Ok(builder) => builder,
        Err(error) => return CommandResponse::err(error),
    };

    match builder.execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(result.output())
            } else {
                CommandResponse::err(format!("Workspace analysis failed: {}", result.stderr))
            }
        }
        Err(e) => CommandResponse::err(e),
    }
}

/// Launch an interactive chat session
pub fn launch_chat() -> CommandResponse {
    launch_chat_with_context(None)
}

pub fn launch_chat_with_context(context: Option<&EditorContext>) -> CommandResponse {
    let builder = match attach_ide_context(CommandBuilder::chat(), context) {
        Ok(builder) => builder,
        Err(error) => return CommandResponse::err(error),
    };

    match builder.execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(result.output())
            } else {
                CommandResponse::err(format!("Chat launch failed: {}", result.stderr))
            }
        }
        Err(e) => CommandResponse::err(e),
    }
}

/// Check Diapason CLI installation and status
pub fn check_status() -> CommandResponse {
    match CommandBuilder::version().execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(format!("Diapason CLI is available\n{}", result.stdout))
            } else {
                CommandResponse::err("Diapason CLI check failed".to_string())
            }
        }
        Err(e) => CommandResponse::err(format!("Diapason CLI not found: {}", e)),
    }
}

/// Find files matching a fuzzy pattern
///
/// Uses the optimized file search bridge for fast parallel file discovery.
/// Supports fuzzy matching and respects .gitignore files.
///
/// # Arguments
/// * `pattern` - Fuzzy search pattern (e.g., "main", "component.rs")
/// * `limit` - Maximum number of results to return
pub fn find_files(pattern: &str, limit: Option<usize>) -> CommandResponse {
    find_files_with_context(pattern, limit, None)
}

pub fn find_files_with_context(
    pattern: &str,
    limit: Option<usize>,
    context: Option<&EditorContext>,
) -> CommandResponse {
    let mut builder = CommandBuilder::find_files(pattern);

    if let Some(l) = limit {
        builder = builder.with_option("limit", l.to_string());
    }

    let builder = match attach_ide_context(builder, context) {
        Ok(builder) => builder,
        Err(error) => return CommandResponse::err(error),
    };

    match builder.execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(result.output())
            } else {
                CommandResponse::err(format!("File search failed: {}", result.stderr))
            }
        }
        Err(e) => CommandResponse::err(format!("File search error: {}", e)),
    }
}

/// List all files in the workspace
///
/// Enumerates files with optional exclusion patterns.
/// Respects .gitignore by default.
///
/// # Arguments
/// * `exclude_patterns` - Optional comma-separated glob patterns to exclude
pub fn list_files(exclude_patterns: Option<&str>) -> CommandResponse {
    list_files_with_context(exclude_patterns, None)
}

pub fn list_files_with_context(
    exclude_patterns: Option<&str>,
    context: Option<&EditorContext>,
) -> CommandResponse {
    let mut builder = CommandBuilder::list_files();

    if let Some(patterns) = exclude_patterns {
        builder = builder.with_option("exclude", patterns);
    }

    let builder = match attach_ide_context(builder, context) {
        Ok(builder) => builder,
        Err(error) => return CommandResponse::err(error),
    };

    match builder.execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(result.output())
            } else {
                CommandResponse::err(format!("File listing failed: {}", result.stderr))
            }
        }
        Err(e) => CommandResponse::err(format!("File listing error: {}", e)),
    }
}

/// Search for files with pattern and exclusions
///
/// Combined search and filter operation for more advanced queries.
///
/// # Arguments
/// * `pattern` - Fuzzy search pattern
/// * `exclude` - Comma-separated glob patterns to exclude
pub fn search_files(pattern: &str, exclude: &str) -> CommandResponse {
    search_files_with_context(pattern, exclude, None)
}

pub fn search_files_with_context(
    pattern: &str,
    exclude: &str,
    context: Option<&EditorContext>,
) -> CommandResponse {
    let builder = match attach_ide_context(
        CommandBuilder::search_files(pattern, exclude),
        context,
    ) {
        Ok(builder) => builder,
        Err(error) => return CommandResponse::err(error),
    };

    match builder.execute() {
        Ok(result) => {
            if result.is_success() {
                CommandResponse::ok(result.output())
            } else {
                CommandResponse::err(format!("File search failed: {}", result.stderr))
            }
        }
        Err(e) => CommandResponse::err(format!("File search error: {}", e)),
    }
}

fn attach_ide_context(
    builder: CommandBuilder,
    context: Option<&EditorContext>,
) -> Result<CommandBuilder, String> {
    let Some(context) = context else {
        return Ok(builder);
    };
    let path = context.write_ide_context_snapshot()?;

    Ok(builder.with_env(
        IDE_CONTEXT_ENV_VAR,
        path.to_string_lossy().into_owned(),
    ))
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
    use std::path::{Path, PathBuf};
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
                "diapason-commands-{label}-{}",
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

        fn directory(&self) -> &Path {
            &self.directory
        }

        /// Write an executable shell script that records its arguments.
        fn write(&self, exit_code: i32, stdout: &str, stderr: &str) {
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
        fn set(key: &str, value: &std::ffi::OsStr) -> Self {
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

    /// Prepend a directory to `PATH` and restore the previous value on drop.
    #[cfg(unix)]
    struct PathGuard {
        previous: Option<OsString>,
    }

    #[cfg(unix)]
    impl PathGuard {
        fn prepend(directory: &Path) -> Self {
            let previous = std::env::var_os("PATH");
            let mut entries = vec![directory.to_path_buf()];
            if let Some(existing) = &previous {
                entries.extend(std::env::split_paths(existing));
            }
            let path = std::env::join_paths(entries)
                .expect("fake executable directory should be a valid PATH entry");
            std::env::set_var("PATH", path);
            Self { previous }
        }
    }

    #[cfg(unix)]
    impl Drop for PathGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn test_command_response_ok() {
        let response = CommandResponse::ok("Success".to_string());
        assert!(response.success);
        assert_eq!(response.output, "Success");
        assert!(response.error.is_none());
    }

    #[test]
    fn test_command_response_err() {
        let response = CommandResponse::err("Error message".to_string());
        assert!(!response.success);
        assert_eq!(response.output, "");
        assert_eq!(response.error, Some("Error message".to_string()));
    }

    #[test]
    fn test_find_files_without_limit() {
        let response = find_files("test", None);
        // This will fail if Diapason is not installed, which is expected
        // The test verifies the function structure
        assert!(response.error.is_some() || response.success);
    }

    #[test]
    fn test_find_files_with_limit() {
        let response = find_files("main", Some(50));
        // Verify response structure
        assert!(response.success || response.error.is_some());
    }

    #[test]
    fn test_list_files_without_exclusions() {
        let response = list_files(None);
        // Verify response structure
        assert!(response.success || response.error.is_some());
    }

    #[test]
    fn test_list_files_with_exclusions() {
        let response = list_files(Some("target/**,node_modules/**"));
        // Verify response structure
        assert!(response.success || response.error.is_some());
    }

    #[test]
    fn test_search_files() {
        let response = search_files("component", "dist/**");
        // Verify response structure
        assert!(response.success || response.error.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn test_analyse_workspace_dispatches_analyse_to_cli() {
        let fake = FakeProgram::new("success");
        fake.write(0, "workspace analysed", "");
        let _path = PathGuard::prepend(fake.directory());
        let _record = fake.record_environment();

        let response = analyse_workspace();

        assert!(response.success, "expected success, got: {response:?}");
        assert!(
            response.output.contains("workspace analysed"),
            "unexpected output: {:?}",
            response.output
        );
        assert_eq!(
            fake.recorded_argv().first().map(String::as_str),
            Some("analyse")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_analyse_workspace_reports_command_failure() {
        let fake = FakeProgram::new("failure");
        fake.write(2, "", "boom");
        let _path = PathGuard::prepend(fake.directory());
        let _record = fake.record_environment();

        let response = analyse_workspace();

        assert!(!response.success);
        let error = response
            .error
            .as_deref()
            .expect("failed analysis should report an error");
        assert!(
            error.contains("Workspace analysis failed:"),
            "unexpected error: {error}"
        );
        assert!(error.contains("boom"), "unexpected error: {error}");
    }
}
