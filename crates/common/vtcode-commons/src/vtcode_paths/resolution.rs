//! Environment-driven root resolution and validation.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use super::{APP, VtCodePaths, native_roots};

impl VtCodePaths {
    /// Resolves paths from the process environment.
    ///
    /// # Errors
    /// Returns an error when no absolute home directory or required storage root can be resolved.
    pub fn from_env() -> Result<Self> {
        Self::from_environment_os(&std::env::vars_os().collect())
    }

    /// Alias for [`Self::from_env`], useful during startup resolution.
    ///
    /// # Errors
    /// Returns an error when no absolute home directory or required storage root can be resolved.
    pub fn resolve() -> Result<Self> {
        Self::from_env()
    }

    /// Resolves paths from an explicit environment, suitable for deterministic tests.
    ///
    /// # Errors
    /// Returns an error when no absolute home directory or required storage root can be resolved.
    pub fn from_environment(environment: &[(&str, &str)]) -> Result<Self> {
        Self::from_environment_os(
            &environment
                .iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                .collect(),
        )
    }

    fn from_environment_os(environment: &BTreeMap<OsString, OsString>) -> Result<Self> {
        let home_dir = environment
            .get(OsStr::new("HOME"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(dirs::home_dir)
            .ok_or_else(|| anyhow!("could not determine the user home directory"))?;
        let native = native_roots(&home_dir)?;
        let home_override = env_path(environment, "VTCODE_HOME");
        if let Some(path) = &home_override {
            validate_absolute("VTCODE_HOME", path)?;
        }
        let legacy_home_dir = home_override.clone().unwrap_or_else(|| home_dir.join(".vtcode"));

        let config_dir = match env_path(environment, "VTCODE_CONFIG") {
            Some(path) => {
                validate_absolute("VTCODE_CONFIG", &path)?;
                path
            }
            None => xdg_app_dir(environment, "XDG_CONFIG_HOME", &native.config_dir)?,
        };
        let data_dir = match env_path(environment, "VTCODE_DATA") {
            Some(path) => {
                validate_absolute("VTCODE_DATA", &path)?;
                path
            }
            None => xdg_app_dir(environment, "XDG_DATA_HOME", &native.data_dir)?,
        };
        let state_dir = xdg_app_dir(environment, "XDG_STATE_HOME", &native.state_dir)?;
        let cache_dir = xdg_app_dir(environment, "XDG_CACHE_HOME", &native.cache_dir)?;
        let runtime_dir = match () {
            _ if is_xdg_platform() => match env_path(environment, "XDG_RUNTIME_DIR") {
                Some(path) if path.is_absolute() => path.join(APP),
                None => state_dir.join("runtime"),
                Some(_) => state_dir.join("runtime"),
            },
            _ => native.runtime_dir.unwrap_or_else(|| state_dir.join("runtime")),
        };
        for (name, path) in [
            ("configuration directory", &config_dir),
            ("data directory", &data_dir),
            ("state directory", &state_dir),
            ("cache directory", &cache_dir),
            ("runtime directory", &runtime_dir),
        ] {
            validate_absolute(name, path)?;
        }
        Ok(Self {
            config_dir,
            data_dir,
            state_dir,
            cache_dir,
            runtime_dir,
            executable_dir: executable_dir(environment, native.executable_dir)?,
            system_config_dirs: system_config_dirs(environment)?,
            system_data_dirs: system_data_dirs(environment)?,
            legacy_home_dir,
        })
    }
}

fn env_path(environment: &BTreeMap<OsString, OsString>, name: &str) -> Option<PathBuf> {
    environment
        .get(OsStr::new(name))
        .filter(|value| !value.is_empty())
        .and_then(|value| {
            if let Some(text) = value.to_str() {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
            } else {
                Some(PathBuf::from(value))
            }
        })
}
fn xdg_app_dir(environment: &BTreeMap<OsString, OsString>, name: &str, native: &Path) -> Result<PathBuf> {
    if is_xdg_platform()
        && let Some(path) = env_path(environment, name)
        && path.is_absolute()
    {
        return Ok(path.join(APP));
    }
    Ok(native.to_path_buf())
}
fn executable_dir(environment: &BTreeMap<OsString, OsString>, native: PathBuf) -> Result<PathBuf> {
    if is_xdg_platform()
        && let Some(path) = env_path(environment, "XDG_BIN_HOME")
        && path.is_absolute()
    {
        return Ok(path);
    }
    Ok(native)
}
fn system_config_dirs(environment: &BTreeMap<OsString, OsString>) -> Result<Vec<PathBuf>> {
    if !is_xdg_platform() {
        return Ok(Vec::new());
    }
    let configured = environment.get(OsStr::new("XDG_CONFIG_DIRS")).map(OsString::as_os_str);
    let mut paths = configured
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        paths.push(PathBuf::from("/etc/xdg"));
    }
    Ok(paths)
}
fn system_data_dirs(environment: &BTreeMap<OsString, OsString>) -> Result<Vec<PathBuf>> {
    if !is_xdg_platform() {
        return Ok(Vec::new());
    }
    let configured = environment.get(OsStr::new("XDG_DATA_DIRS")).map(OsString::as_os_str);
    let mut paths = configured
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        paths.extend([PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);
    }
    Ok(paths)
}
fn validate_absolute(name: &str, path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        bail!("{name} must be an absolute path, got '{}'", path.display())
    }
}
const fn is_xdg_platform() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))
}
