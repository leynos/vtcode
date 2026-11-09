//! Native and XDG-compliant storage paths for Diapason.

use std::path::{Path, PathBuf};
use std::time::Duration;

mod accessors;
#[path = "migration/mod.rs"]
mod migration;
mod private_files;
mod resolution;
mod storage;
#[cfg(test)]
mod tests;

pub use migration::{
    LegacyMigrator, MigrationEntry, MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason,
};

use private_files::{create_private_new_file, ensure_migration_dir, ensure_private_dir, open_no_follow};

const APP: &str = "diapason";
const MARKER: &str = "legacy-v1.complete";
const PRIVATE_FILE_LOCK_ATTEMPTS: usize = 100;
const PRIVATE_FILE_LOCK_DELAY: Duration = Duration::from_millis(10);

struct NativeRoots {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
    executable_dir: PathBuf,
}

fn native_roots(home_dir: &Path) -> NativeRoots {
    NativeRoots {
        config_dir: home_dir.join(".config").join(APP),
        data_dir: home_dir.join(".local/share").join(APP),
        state_dir: home_dir.join(".local/state").join(APP),
        cache_dir: home_dir.join(".cache").join(APP),
        executable_dir: home_dir.join(".local/bin"),
    }
}

/// Validated native/XDG storage roots and typed Diapason child paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiapasonPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
    runtime_dir: PathBuf,
    executable_dir: PathBuf,
    system_config_dirs: Vec<PathBuf>,
    system_data_dirs: Vec<PathBuf>,
    legacy_home_dir: PathBuf,
}
