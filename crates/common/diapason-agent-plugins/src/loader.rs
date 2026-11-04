use std::path::{Path, PathBuf};

use crate::discovery::LoadedPlugin;
use crate::errors::PluginError;

use dirs::home_dir;
/// Abstraction for loading Agent Plugins from a source.
///
/// This trait isolates the loading mechanism from the rest of the system,
/// allowing the next generation to swap the filesystem implementation for
/// network-loaded, cached, or sandboxed loaders without changing callers.
pub trait PluginLoader {
    /// Load a plugin from the given path.
    fn load(&self, path: &Path) -> Result<LoadedPlugin, PluginError>;
}

/// Ordered plugin discovery roots: the workspace project root first, then the
/// user home root. Shared by the CLI `plugins` subcommands and the core MCP
/// discovery so both layers agree on where plugins live.
pub fn plugin_roots_for(workspace: &Path) -> Vec<PathBuf> {
    vec![
        workspace.join(".agents/plugins"),
        home_dir().map(|h| h.join(".agents/plugins")).unwrap_or_default(),
    ]
}

/// Default filesystem-based plugin loader.
///
/// This is the current production implementation. It reads `plugin.json`,
/// `skills/`, and `mcp.json` from the local filesystem.
pub struct FileSystemPluginLoader {
    _private: (),
}

impl FileSystemPluginLoader {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for FileSystemPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginLoader for FileSystemPluginLoader {
    fn load(&self, path: &Path) -> Result<LoadedPlugin, PluginError> {
        LoadedPlugin::load_from_dir(path)
    }
}

/// Abstraction for validating loaded plugins.
///
/// Callers should validate plugins after loading and before use.
/// The default implementation accepts all valid plugins.
pub trait PluginValidator {
    /// Validate a loaded plugin. Returns `Ok(())` if the plugin is acceptable.
    fn validate(&self, plugin: &LoadedPlugin) -> Result<(), PluginError>;
}

/// Default validator that accepts all successfully-loaded plugins.
///
/// Additional checks (trust model, signature, etc.) can be layered by
/// wrapping this validator with a decorator.
pub struct DefaultPluginValidator;

impl PluginValidator for DefaultPluginValidator {
    fn validate(&self, _plugin: &LoadedPlugin) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Outcome of a plugin installation.
pub struct InstalledPlugin {
    pub name: String,
    pub path: PathBuf,
    pub loaded: LoadedPlugin,
}

/// Abstraction for installing and removing plugins.
///
/// This isolates installation mechanics (git clone, local copy, etc.)
/// from the CLI layer, enabling test doubles and alternate install targets.
pub trait PluginInstaller {
    /// Install a plugin from `source` with the given `name`.
    fn install(&self, source: &str, name: Option<String>) -> Result<InstalledPlugin, PluginError>;

    /// Remove an installed plugin by `name`.
    fn remove(&self, name: &str) -> Result<(), PluginError>;
}

/// Default filesystem-based installer.
pub struct FileSystemPluginInstaller {
    base_dir: Option<PathBuf>,
}

impl FileSystemPluginInstaller {
    pub fn new() -> Self {
        Self { base_dir: None }
    }

    /// Install under a custom base directory instead of the user home.
    /// Used by tests to keep installs hermetic.
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir: Some(base_dir) }
    }

    fn install_root(&self) -> Result<PathBuf, PluginError> {
        match &self.base_dir {
            Some(base) => Ok(base.clone()),
            None => home_dir().ok_or_else(|| {
                PluginError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve home directory"))
            }),
        }
    }
}

impl Default for FileSystemPluginInstaller {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginInstaller for FileSystemPluginInstaller {
    fn install(&self, source: &str, name: Option<String>) -> Result<InstalledPlugin, PluginError> {
        let target_name = name.unwrap_or_else(|| {
            Path::new(source)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("plugin")
                .to_string()
        });

        // Reject names that could escape the plugins root (e.g. "../evil" or
        // absolute paths). The name is user-supplied via `--name`.
        crate::manifest::PluginManifest::validate_name(&target_name).map_err(PluginError::InvalidName)?;

        let install_dir = self.install_root()?.join(".agents/plugins").join(&target_name);

        if install_dir.exists() {
            return Err(PluginError::AlreadyInstalled(format!(
                "'{target_name}' already exists at {}. Remove it with 'diapason plugins remove {target_name}', or install under a different name with '--name <id>'",
                install_dir.display()
            )));
        }

        if source.starts_with("https://")
            || source.starts_with("http://")
            || source.starts_with("git@")
            || source.starts_with("ssh://")
            || source.ends_with(".git")
        {
            let status = std::process::Command::new("git")
                .args(["clone", "--depth=1", source, &install_dir.to_string_lossy()])
                .status()
                .map_err(PluginError::Io)?;
            if !status.success() {
                drop(std::fs::remove_dir_all(&install_dir));
                return Err(PluginError::Io(std::io::Error::other(format!("git clone failed for {}", source))));
            }
        } else {
            let src = Path::new(source);
            if !src.is_dir() {
                return Err(PluginError::InvalidManifest(format!("source is not a directory or git URL: {}", source)));
            }
            if !src.join("plugin.json").is_file() {
                return Err(PluginError::InvalidManifest(format!(
                    "source directory does not contain a valid plugin.json: {}",
                    source
                )));
            }
            std::fs::create_dir_all(&install_dir).map_err(PluginError::Io)?;
            copy_dir_all(src, &install_dir).map_err(PluginError::Io)?;
        }

        let loaded = LoadedPlugin::load_from_dir(&install_dir).inspect_err(|_| {
            drop(std::fs::remove_dir_all(&install_dir));
        })?;
        Ok(InstalledPlugin { name: target_name, path: install_dir, loaded })
    }

    fn remove(&self, name: &str) -> Result<(), PluginError> {
        // Reject names that could escape the plugins root. Without this,
        // `remove("../../.config/x")` would delete arbitrary directories.
        crate::manifest::PluginManifest::validate_name(name).map_err(PluginError::InvalidName)?;

        let install_dir = self.install_root()?.join(".agents/plugins").join(name);
        if !install_dir.exists() {
            return Err(PluginError::NotInstalled(format!(
                "'{name}' is not installed at {}. Install it with 'diapason plugins add <source>'",
                install_dir.display()
            )));
        }
        if !install_dir.join("plugin.json").is_file() {
            return Err(PluginError::InvalidManifest(format!(
                "not a valid agent plugin (missing plugin.json): {}",
                install_dir.display()
            )));
        }
        std::fs::remove_dir_all(&install_dir).map_err(PluginError::Io)
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_dir_all_inner(src, dst, &mut Vec::new())
}

fn copy_dir_all_inner(src: &Path, dst: &Path, ancestors: &mut Vec<PathBuf>) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;

    // Guard against symlink cycles: if we are revisiting a directory we are
    // already an ancestor of, abort instead of recursing forever.
    let canonical = diapason_commons::canonicalize(src)?;
    if ancestors.contains(&canonical) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("symlink cycle detected while copying {}", src.display()),
        ));
    }
    ancestors.push(canonical);

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let dst_path = dst.join(&file_name);

        // Do not copy the source plugin's own VCS metadata; a local checkout
        // should install the plugin, not its history. Other dotfiles (e.g.
        // .npmrc, .env) are preserved because they may be required at runtime.
        if name == ".git" {
            continue;
        }

        let meta = std::fs::symlink_metadata(&src_path)?;
        if meta.file_type().is_symlink() {
            // Copy symlinks as their link target, not the linked content, so
            // plugin-internal symlinks stay relative and absolute symlinks
            // cannot pull data out of the source into the install tree.
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&src_path)?;
                std::os::unix::fs::symlink(target, &dst_path)?;
            }
            #[cfg(not(unix))]
            let _copied_bytes = std::fs::copy(&src_path, &dst_path)?;
        } else if meta.is_dir() {
            copy_dir_all_inner(&src_path, &dst_path, ancestors)?;
        } else {
            let _copied_bytes = std::fs::copy(&src_path, &dst_path)?;
        }
    }

    drop(ancestors.pop());
    Ok(())
}
