//! Shared fixture for extracted `Diapason` path regression test modules.

use super::DiapasonPaths;

fn migration_paths(temp: &tempfile::TempDir) -> DiapasonPaths {
    DiapasonPaths {
        config_dir: temp.path().join("config"),
        data_dir: temp.path().join("data"),
        state_dir: temp.path().join("state"),
        cache_dir: temp.path().join("cache"),
        runtime_dir: temp.path().join("runtime"),
        executable_dir: temp.path().join("bin"),
        system_config_dirs: Vec::new(),
        system_data_dirs: Vec::new(),
        legacy_home_dir: temp.path().join("legacy"),
    }
}

mod behaviour;
mod resolution;
