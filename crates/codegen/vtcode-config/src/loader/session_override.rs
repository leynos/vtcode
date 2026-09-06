//! Process-wide explicit config-file override for a session.
//!
//! When the user launches vtcode with `--config PATH` or
//! `VTCODE_CONFIG_PATH`, every later configuration reload must honour that
//! explicit file instead of silently drifting back to the default layer
//! hierarchy. The resolved path is captured once during startup and stored
//! here as a session snapshot, so runtime reloads via
//! [`ConfigManager::load_from_workspace`] stay deterministic.
//!
//! Reloads reuse this stored snapshot rather than re-reading the
//! environment, which keeps them deterministic and matches `--config`
//! semantics.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

use super::manager::ConfigManager;

static EXPLICIT_CONFIG_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<PathBuf>> {
    EXPLICIT_CONFIG_PATH.get_or_init(|| Mutex::new(None))
}

/// Capture the session's explicit config-file override.
///
/// Called during startup after the CLI/env path has been resolved to an
/// absolute file path. Passing `None` clears the override and invalidates the
/// workspace config cache so subsequent loads return to the default layer
/// hierarchy instead of a stale override-loaded manager.
///
/// This is process-global and intended to be set once per process at startup;
/// changing it mid-session is supported for tests and reloads, but consumers
/// must not race concurrent calls.
pub fn set_explicit_config_path(path: Option<PathBuf>) {
    {
        let mut guard = cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = path;
    }
    // A cleared or changed override must not leave a stale manager in the
    // workspace cache: the cache key is the canonical workspace only, so an
    // override-loaded manager would otherwise leak into later default loads
    // for any workspace. Invalidate every entry.
    ConfigManager::invalidate_all_workspace_cache();
}

/// Return the session's explicit config-file override, if one was captured.
pub fn explicit_config_path() -> Option<PathBuf> {
    let guard = cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.clone()
}

/// RAII helper for tests: set an override and restore the previous value on drop.
#[cfg(test)]
pub(crate) struct ExplicitConfigPathGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl ExplicitConfigPathGuard {
    pub(crate) fn set(path: Option<PathBuf>) -> Self {
        let previous = explicit_config_path();
        set_explicit_config_path(path);
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for ExplicitConfigPathGuard {
    fn drop(&mut self) {
        set_explicit_config_path(self.previous.take());
    }
}
