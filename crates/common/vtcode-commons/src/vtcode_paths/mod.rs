//! Native and XDG-compliant storage paths for VT Code.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};

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

const APP: &str = "vtcode";
const MARKER: &str = "legacy-v1.complete";
const PRIVATE_FILE_LOCK_ATTEMPTS: usize = 100;
const PRIVATE_FILE_LOCK_DELAY: Duration = Duration::from_millis(10);

struct NativeRoots {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
    runtime_dir: Option<PathBuf>,
    executable_dir: PathBuf,
}

fn native_roots(
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "windows")),
        allow(
            unused_variables,
            reason = "home_dir is only consumed by macOS/Windows root resolution"
        )
    )]
    home_dir: &Path,
) -> Result<NativeRoots> {
    #[cfg(target_os = "macos")]
    {
        let root = dirs::data_local_dir()
            .ok_or_else(|| anyhow!("could not determine the macOS application support directory"))?
            .join("com.vinhnx.vtcode");
        Ok(NativeRoots {
            config_dir: root.clone(),
            data_dir: root.clone(),
            state_dir: root.join("state"),
            cache_dir: dirs::cache_dir()
                .ok_or_else(|| anyhow!("could not determine the macOS cache directory"))?
                .join("com.vinhnx.vtcode"),
            runtime_dir: None,
            executable_dir: home_dir.join(".local/bin"),
        })
    }
    #[cfg(target_os = "windows")]
    {
        let root = dirs::data_dir()
            .ok_or_else(|| anyhow!("could not determine the Windows application data directory"))?
            .join("vinhnx")
            .join(APP);
        Ok(NativeRoots {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            runtime_dir: None,
            executable_dir: root.join("bin"),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(NativeRoots {
            config_dir: home_dir.join(".config").join(APP),
            data_dir: home_dir.join(".local/share").join(APP),
            state_dir: home_dir.join(".local/state").join(APP),
            cache_dir: home_dir.join(".cache").join(APP),
            runtime_dir: None,
            executable_dir: home_dir.join(".local/bin"),
        })
    }
}

/// Validated native/XDG storage roots and typed VT Code child paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtCodePaths {
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
