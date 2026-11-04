//! Shared sandbox context for MCP stdio launches.

use anyhow::{Result, anyhow};
use hashbrown::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use diapason_safety::sandboxing::{CommandSpec, SandboxManager, SandboxPolicy};

/// Sandbox policy and launch context inherited by MCP stdio providers.
#[derive(Debug, Clone)]
pub struct McpSandboxContext {
    policy: SandboxPolicy,
    sandbox_cwd: PathBuf,
    linux_sandbox_executable: Option<PathBuf>,
}

impl McpSandboxContext {
    /// Create a context using the configured workspace as the sandbox cwd.
    #[must_use]
    pub fn new(policy: SandboxPolicy, sandbox_cwd: impl Into<PathBuf>) -> Self {
        Self {
            policy,
            sandbox_cwd: sandbox_cwd.into(),
            linux_sandbox_executable: std::env::var_os("DIAPASON_LINUX_SANDBOX_EXECUTABLE").map(PathBuf::from),
        }
    }

    /// Override the Linux helper path, primarily for embedding applications and tests.
    #[must_use]
    pub fn with_linux_sandbox_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.linux_sandbox_executable = Some(executable.into());
        self
    }

    /// Return the policy carried by this context.
    #[must_use]
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    pub(crate) fn transform_stdio(
        &self,
        program: OsString,
        args: Vec<OsString>,
        working_dir: Option<&Path>,
        env: HashMap<OsString, OsString>,
    ) -> Result<SandboxedStdioCommand> {
        if matches!(self.policy, SandboxPolicy::ExternalSandbox { .. }) {
            return Err(anyhow!("MCP stdio cannot use an external sandbox policy without an external launcher"));
        }

        let cwd = working_dir.map(Path::to_path_buf).unwrap_or_else(|| self.sandbox_cwd.clone());
        let spec = CommandSpec::new(program.clone())
            .with_args(args.iter().map(|arg| arg.to_string_lossy().into_owned()))
            .with_cwd(cwd.clone())
            .with_env(
                env.iter()
                    .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
                    .collect(),
            );
        let exec_env = SandboxManager::new()
            .transform(spec, &self.policy, &cwd, self.linux_sandbox_executable.as_deref())
            .map_err(|error| anyhow!("failed to apply MCP sandbox: {error}"))?;

        Ok(SandboxedStdioCommand {
            program: exec_env.program.into_os_string(),
            args: exec_env.args.into_iter().map(OsString::from).collect(),
            working_dir: exec_env.cwd,
            env: exec_env
                .env
                .into_iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                .collect(),
        })
    }
}

pub(crate) struct SandboxedStdioCommand {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) working_dir: PathBuf,
    pub(crate) env: HashMap<OsString, OsString>,
}
