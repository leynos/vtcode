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

        let path = match self {
            TerminalType::Ghostty => {
                if cfg!(target_os = "windows") {
                    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
                    PathBuf::from(appdata).join("ghostty").join("config")
                } else {
                    home_dir.join(".config").join("ghostty").join("config")
                }
            }
            TerminalType::Kitty => {
                if cfg!(target_os = "windows") {
                    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
                    PathBuf::from(appdata).join("kitty").join("kitty.conf")
                } else {
                    home_dir.join(".config").join("kitty").join("kitty.conf")
                }
            }
            TerminalType::Alacritty => {
                if cfg!(target_os = "windows") {
                    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
                    PathBuf::from(appdata).join("alacritty").join("alacritty.toml")
                } else {
                    home_dir.join(".config").join("alacritty").join("alacritty.toml")
                }
            }
            TerminalType::WezTerm => home_dir.join(".wezterm.lua"),
            TerminalType::TerminalApp => {
                if cfg!(target_os = "macos") {
                    home_dir.join("Library").join("Preferences").join("com.apple.Terminal.plist")
                } else {
                    anyhow::bail!("Terminal.app is only available on macOS")
                }
            }
            TerminalType::Xterm => home_dir.join(".Xresources"),
            TerminalType::Zed => {
                if cfg!(target_os = "windows") {
                    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
                    PathBuf::from(appdata).join("Zed").join("settings.json")
                } else if cfg!(target_os = "macos") {
                    home_dir
                        .join("Library")
                        .join("Application Support")
                        .join("Zed")
                        .join("settings.json")
                } else {
                    home_dir.join(".config").join("zed").join("settings.json")
                }
            }
            TerminalType::Warp => {
                if cfg!(target_os = "macos") {
                    home_dir.join(".warp")
                } else {
                    home_dir.join(".config").join("warp")
                }
            }
            TerminalType::ITerm2 => {
                if cfg!(target_os = "macos") {
                    home_dir.join("Library").join("Preferences").join("com.googlecode.iterm2.plist")
                } else {
                    anyhow::bail!("iTerm2 is only available on macOS")
                }
            }
            TerminalType::VSCode => {
                if cfg!(target_os = "windows") {
                    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
                    PathBuf::from(appdata).join("Code").join("User").join("settings.json")
                } else if cfg!(target_os = "macos") {
                    home_dir
                        .join("Library")
                        .join("Application Support")
                        .join("Code")
                        .join("User")
                        .join("settings.json")
                } else {
                    home_dir.join(".config").join("Code").join("User").join("settings.json")
                }
            }
            TerminalType::WindowsTerminal => {
                if cfg!(target_os = "windows") {
                    let local_appdata =
                        env::var("LOCALAPPDATA").context("LOCALAPPDATA environment variable not set")?;
                    PathBuf::from(local_appdata)
                        .join("Packages")
                        .join("Microsoft.WindowsTerminal_8wekyb3d8bbwe")
                        .join("LocalState")
                        .join("settings.json")
                } else {
                    anyhow::bail!("Windows Terminal is only available on Windows")
                }
            }
            TerminalType::Hyper => home_dir.join(".hyper.js"),
            TerminalType::Tabby => {
                if cfg!(target_os = "windows") {
                    let appdata = env::var("APPDATA").context("APPDATA environment variable not set")?;
                    PathBuf::from(appdata).join("tabby").join("config.yaml")
                } else if cfg!(target_os = "macos") {
                    home_dir
                        .join("Library")
                        .join("Application Support")
                        .join("tabby")
                        .join("config.yaml")
                } else {
                    home_dir.join(".config").join("tabby").join("config.yaml")
                }
            }
            TerminalType::Unknown => {
                anyhow::bail!("Cannot determine config path for unknown terminal")
            }
        };

        Ok(path)
    }
}
