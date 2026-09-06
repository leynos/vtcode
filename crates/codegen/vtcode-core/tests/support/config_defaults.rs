//! Scoped, hermetic defaults for integration tests that load workspace config.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, OwnedMutexGuard};
use vtcode_commons::reference::StaticWorkspacePaths;
use vtcode_config::defaults::{ConfigDefaultsProvider, WorkspacePathsDefaults, install_config_defaults_provider};

static CONFIG_DEFAULTS_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

/// Restores the previous config defaults after a test has loaded an isolated workspace.
pub struct IsolatedConfigDefaultsGuard {
    previous: Arc<dyn ConfigDefaultsProvider>,
    _lock: OwnedMutexGuard<()>,
}

impl IsolatedConfigDefaultsGuard {
    /// Installs workspace-only config defaults until this guard is dropped.
    pub async fn install(workspace_root: &Path) -> Self {
        let lock = Arc::clone(CONFIG_DEFAULTS_LOCK.get_or_init(|| Arc::new(Mutex::new(()))))
            .lock_owned()
            .await;
        let workspace_paths = StaticWorkspacePaths::new(workspace_root, workspace_root.join(".vtcode"));
        let provider: Arc<dyn ConfigDefaultsProvider> = WorkspacePathsDefaults::new(Arc::new(workspace_paths))
            .with_home_paths(Vec::new())
            .with_system_config_paths(Vec::new())
            .build()
            .into();
        let previous = install_config_defaults_provider(provider);

        Self { previous, _lock: lock }
    }
}

impl Drop for IsolatedConfigDefaultsGuard {
    fn drop(&mut self) {
        let _previous = install_config_defaults_provider(Arc::clone(&self.previous));
    }
}
