//! Terminal configuration path resolution for each supported platform.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::TerminalType;

impl TerminalType {
    /// Returns this terminal's configuration file path.
    ///
    /// # Errors
    ///
    /// Returns an error when the home directory or required platform environment
    /// variable is unavailable, or when this terminal has no configuration path
    /// on the current platform.
    pub fn config_path(&self) -> Result<PathBuf> {
        let home_dir = dirs::home_dir().context("Failed to determine home directory")?;
        self.config_path_in(&home_dir)
    }

    /// Resolves this terminal's configuration path beneath `home_dir`.
    fn config_path_in(&self, home_dir: &Path) -> Result<PathBuf> {
        match self {
            TerminalType::Ghostty => appdata_or_xdg_path(home_dir, &["ghostty", "config"]),
            TerminalType::Kitty => appdata_or_xdg_path(home_dir, &["kitty", "kitty.conf"]),
            TerminalType::Alacritty => appdata_or_xdg_path(home_dir, &["alacritty", "alacritty.toml"]),
            TerminalType::WezTerm => Ok(home_dir.join(".wezterm.lua")),
            TerminalType::TerminalApp => {
                macos_only_path(macos_preferences_path(home_dir, "com.apple.Terminal.plist"), "Terminal.app")
            }
            TerminalType::Xterm => Ok(home_dir.join(".Xresources")),
            TerminalType::Zed => zed_config_path(home_dir),
            TerminalType::Warp => Ok(warp_config_path(home_dir)),
            TerminalType::ITerm2 => {
                macos_only_path(macos_preferences_path(home_dir, "com.googlecode.iterm2.plist"), "iTerm2")
            }
            TerminalType::VSCode => vscode_config_path(home_dir),
            TerminalType::WindowsTerminal => windows_terminal_config_path(),
            TerminalType::Hyper => Ok(home_dir.join(".hyper.js")),
            TerminalType::Tabby => tabby_config_path(home_dir),
            TerminalType::Unknown => {
                anyhow::bail!("Cannot determine config path for unknown terminal")
            }
        }
    }
}

/// Returns `%APPDATA%` joined with `components`.
fn appdata_path(components: &[&str]) -> Result<PathBuf> {
    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
    Ok(join_components(PathBuf::from(appdata), components))
}

/// Returns `<home>/.config` joined with `components`.
fn xdg_path(home_dir: &Path, components: &[&str]) -> PathBuf {
    join_components(home_dir.join(".config"), components)
}

/// Returns the Windows application-data path, or the XDG path elsewhere.
fn appdata_or_xdg_path(home_dir: &Path, components: &[&str]) -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        appdata_path(components)
    } else {
        Ok(xdg_path(home_dir, components))
    }
}

/// Returns `<home>/Library/Application Support` joined with `components`.
fn macos_application_support_path(home_dir: &Path, components: &[&str]) -> PathBuf {
    join_components(home_dir.join("Library").join("Application Support"), components)
}

/// Returns `<home>/Library/Preferences/<file>`.
fn macos_preferences_path(home_dir: &Path, file: &str) -> PathBuf {
    home_dir.join("Library").join("Preferences").join(file)
}

/// Returns `path` when running on macOS, or the platform availability error otherwise.
fn macos_only_path(path: PathBuf, terminal: &str) -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(path)
    } else {
        anyhow::bail!("{terminal} is only available on macOS")
    }
}

/// Joins `components` onto `base` in order.
fn join_components(base: PathBuf, components: &[&str]) -> PathBuf {
    components.iter().fold(base, |path, component| path.join(component))
}

/// Resolves Zed's settings path for the current platform.
fn zed_config_path(home_dir: &Path) -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        appdata_path(&["Zed", "settings.json"])
    } else if cfg!(target_os = "macos") {
        Ok(macos_application_support_path(home_dir, &["Zed", "settings.json"]))
    } else {
        Ok(xdg_path(home_dir, &["zed", "settings.json"]))
    }
}

/// Resolves Warp's configuration directory for the current platform.
fn warp_config_path(home_dir: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home_dir.join(".warp")
    } else {
        xdg_path(home_dir, &["warp"])
    }
}

/// Resolves VS Code's settings path for the current platform.
fn vscode_config_path(home_dir: &Path) -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        appdata_path(&["Code", "User", "settings.json"])
    } else if cfg!(target_os = "macos") {
        Ok(macos_application_support_path(home_dir, &["Code", "User", "settings.json"]))
    } else {
        Ok(xdg_path(home_dir, &["Code", "User", "settings.json"]))
    }
}

/// Resolves Windows Terminal's settings path.
fn windows_terminal_config_path() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        let local_appdata = env::var("LOCALAPPDATA").context("LOCALAPPDATA environment variable not set")?;
        Ok(join_components(
            PathBuf::from(local_appdata),
            &[
                "Packages",
                "Microsoft.WindowsTerminal_8wekyb3d8bbwe",
                "LocalState",
                "settings.json",
            ],
        ))
    } else {
        anyhow::bail!("Windows Terminal is only available on Windows")
    }
}

/// Resolves Tabby's configuration path for the current platform.
fn tabby_config_path(home_dir: &Path) -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        appdata_path(&["tabby", "config.yaml"])
    } else if cfg!(target_os = "macos") {
        Ok(macos_application_support_path(home_dir, &["tabby", "config.yaml"]))
    } else {
        Ok(xdg_path(home_dir, &["tabby", "config.yaml"]))
    }
}
