//! Fail-closed workspace admission for client-selected ACP session paths.
use crate::acp;
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

use super::session::AcpSessionWorkspace;

#[derive(Clone, Debug)]
pub(crate) struct AcpWorkspacePolicy {
    launch_context: PathBuf,
    allowed_roots: Vec<PathBuf>,
    home: Option<PathBuf>,
}

impl AcpWorkspacePolicy {
    pub(crate) async fn new(launch_workspace: &Path, configured_roots: &[PathBuf]) -> Result<Self> {
        let home = match dirs::home_dir() {
            Some(home) => Some(canonical_existing_directory(&home, "user home directory").await?),
            None => None,
        };
        let launch_root = canonical_existing_directory(launch_workspace, "ACP launch workspace").await?;

        let mut allowed_roots = Vec::new();
        if !is_broad_scope(&launch_root, home.as_deref()) {
            allowed_roots.push(launch_root.clone());
        }
        for configured_root in configured_roots {
            let root = canonical_existing_directory(configured_root, "configured ACP allowed workspace root").await?;
            ensure!(
                !is_broad_scope(&root, home.as_deref()),
                "configured ACP allowed workspace root cannot be a filesystem root or the home directory: {}",
                root.display()
            );
            if !allowed_roots.contains(&root) {
                allowed_roots.push(root);
            }
        }

        Ok(Self { launch_context: launch_root, allowed_roots, home })
    }

    /// Return the canonical process launch directory for startup context.
    ///
    /// This path is not necessarily a session workspace and is not necessarily
    /// an allowed root. In particular, broad launch directories remain usable
    /// for startup configuration while `resolve_client_cwd` rejects them for
    /// client-created sessions.
    pub(crate) fn launch_context(&self) -> &Path {
        &self.launch_context
    }

    pub(crate) async fn resolve_client_cwd(
        &self,
        requested_workspace: &Path,
    ) -> Result<AcpSessionWorkspace, acp::Error> {
        let workspace = AcpSessionWorkspace::from_client_cwd(requested_workspace).await?;
        if is_broad_scope(workspace.path(), self.home.as_deref()) {
            return Err(
                acp::Error::invalid_params().data("ACP session cwd cannot be a filesystem root or the home directory")
            );
        }
        if !self.allowed_roots.iter().any(|root| workspace.path().starts_with(root)) {
            return Err(acp::Error::invalid_params().data(format!(
                "ACP session cwd '{}' is outside the configured allowed workspaces",
                workspace.path().display()
            )));
        }
        Ok(workspace)
    }
}

async fn canonical_existing_directory(path: &Path, label: &str) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "{label} must be an absolute path: {}", path.display());
    let canonical = vtcode_commons::fs::canonicalize_with_context_async(path, label)
        .await
        .with_context(|| format!("canonicalizing {label} '{}'", path.display()))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .with_context(|| format!("inspecting {label} '{}'", canonical.display()))?;
    ensure!(metadata.is_dir(), "{label} is not a directory: {}", canonical.display());
    Ok(canonical)
}

fn is_broad_scope(path: &Path, home: Option<&Path>) -> bool {
    path.parent().is_none() || home.is_some_and(|home| path == home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;

    #[tokio::test]
    async fn allows_only_the_launch_workspace_and_configured_roots() {
        let fixture = TempDir::new().expect("temporary workspace policy fixture");
        let launch = fixture.path().join("launch");
        let configured = fixture.path().join("configured");
        let configured_child = configured.join("project");
        let unrelated = fixture.path().join("unrelated");
        for path in [&launch, &configured_child, &unrelated] {
            tokio::fs::create_dir_all(path).await.expect("create policy test directory");
        }

        let policy = AcpWorkspacePolicy::new(&launch, std::slice::from_ref(&configured))
            .await
            .expect("construct allowed-workspace policy");
        let launch_workspace = policy.resolve_client_cwd(&launch).await.expect("launch workspace is allowed");
        let configured_workspace = policy
            .resolve_client_cwd(&configured_child)
            .await
            .expect("configured root admits its child workspace");

        assert_eq!(
            launch_workspace.path(),
            vtcode_commons::fs::canonicalize_with_context_async(&launch, "ACP launch test workspace")
                .await
                .expect("canonical launch path")
        );
        assert_eq!(
            configured_workspace.path(),
            vtcode_commons::fs::canonicalize_with_context_async(&configured_child, "configured child test workspace")
                .await
                .expect("canonical configured child path")
        );
        assert!(policy.resolve_client_cwd(&unrelated).await.is_err());
    }

    #[tokio::test]
    async fn rejects_filesystem_root_and_home_as_session_workspaces() {
        let launch = TempDir::new().expect("temporary launch workspace");
        let policy = AcpWorkspacePolicy::new(launch.path(), &[])
            .await
            .expect("construct allowed-workspace policy");
        let filesystem_root = std::env::current_dir()
            .expect("resolve current directory")
            .ancestors()
            .last()
            .expect("filesystem root exists")
            .to_path_buf();

        assert!(policy.resolve_client_cwd(&filesystem_root).await.is_err());
        if let Some(home) = dirs::home_dir() {
            assert!(policy.resolve_client_cwd(&home).await.is_err());
        }
    }

    #[tokio::test]
    async fn allows_broad_launch_context_without_admitting_it_as_a_session_root() {
        let filesystem_root = std::env::current_dir()
            .expect("resolve current directory")
            .ancestors()
            .last()
            .expect("filesystem root exists")
            .to_path_buf();
        let policy = AcpWorkspacePolicy::new(&filesystem_root, &[])
            .await
            .expect("filesystem root is valid launch context");

        assert_eq!(policy.launch_context(), filesystem_root.as_path());
        assert!(policy.allowed_roots.is_empty());
        assert!(policy.resolve_client_cwd(&filesystem_root).await.is_err());

        if let Some(home) = dirs::home_dir() {
            let policy = AcpWorkspacePolicy::new(&home, &[])
                .await
                .expect("home directory is valid launch context");

            assert!(policy.allowed_roots.is_empty());
            assert!(policy.resolve_client_cwd(policy.launch_context()).await.is_err());
        }
    }

    #[tokio::test]
    async fn rejects_unconfigured_and_nonexistent_roots_during_startup() {
        let launch = TempDir::new().expect("temporary launch workspace");
        let relative_root = PathBuf::from("relative-workspace");
        assert!(AcpWorkspacePolicy::new(launch.path(), &[relative_root]).await.is_err());

        let missing_root = launch.path().join("missing");
        assert!(AcpWorkspacePolicy::new(launch.path(), &[missing_root]).await.is_err());
    }
}
