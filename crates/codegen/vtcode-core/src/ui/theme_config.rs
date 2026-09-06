//! Theme Configuration File Support
//!
//! Parses custom .vtcode/theme.toml files with Git/LS-style syntax for colours.
//! This allows users to customize colours beyond system defaults.

use crate::utils::CachedStyleParser;
use crate::utils::file_utils::read_file_with_context_sync;
use anstyle::Style as AnsiStyle;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Theme configuration that can be loaded from a .vtcode/theme.toml file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// Colours for CLI elements
    #[serde(default)]
    pub cli: CliColours,

    /// Colours for diff rendering
    #[serde(default)]
    pub diff: DiffColours,

    /// Colours for status output
    #[serde(default)]
    pub status: StatusColours,

    /// Colours for file types (LS_COLORS-style)
    #[serde(default)]
    pub files: FileColours,
}

impl ThemeConfig {
    /// Load theme configuration from a TOML file
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = read_file_with_context_sync(path, "theme file")
            .with_context(|| format!("Failed to read theme file: {}", path.display()))?;

        let config: ThemeConfig =
            toml::from_str(&content).with_context(|| format!("Failed to parse theme file: {}", path.display()))?;

        Ok(config)
    }

    /// Create default theme configuration
    pub fn new() -> Self {
        Self::default_config()
    }

    /// Returns a default configuration
    fn default_config() -> Self {
        Self {
            cli: CliColours::default(),
            diff: DiffColours::default(),
            status: StatusColours::default(),
            files: FileColours::default(),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Colours for CLI elements like prompts, messages, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliColours {
    /// Colour for success messages
    #[serde(default = "default_cli_success")]
    pub success: String,

    /// Colour for error messages
    #[serde(default = "default_cli_error")]
    pub error: String,

    /// Colour for warning messages
    #[serde(default = "default_cli_warning")]
    pub warning: String,

    /// Colour for info messages
    #[serde(default = "default_cli_info")]
    pub info: String,

    /// Colour for prompt text
    #[serde(default = "default_cli_prompt")]
    pub prompt: String,
}

impl Default for CliColours {
    fn default() -> Self {
        Self {
            success: "green".into(),
            error: "red".into(),
            warning: "red".into(),
            info: "cyan".into(),
            prompt: "bold cyan".into(),
        }
    }
}

/// Colours for diff rendering
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffColours {
    /// Colour for added lines in diff
    #[serde(default = "default_diff_new")]
    pub new: String,

    /// Colour for removed lines in diff
    #[serde(default = "default_diff_old")]
    pub old: String,

    /// Colour for context/unchanged lines in diff
    #[serde(default = "default_diff_context")]
    pub context: String,

    /// Colour for diff headers
    #[serde(default = "default_diff_header")]
    pub header: String,

    /// Colour for diff metadata
    #[serde(default = "default_diff_meta")]
    pub meta: String,

    /// Colour for diff fragment indicators
    #[serde(default = "default_diff_frag")]
    pub frag: String,
}

impl Default for DiffColours {
    fn default() -> Self {
        Self {
            new: "green".into(),
            old: "red".into(),
            context: "dim".into(),
            header: "bold cyan".into(),
            meta: "cyan".into(),
            frag: "cyan".into(),
        }
    }
}

/// Colours for status output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusColours {
    /// Colour for added files
    #[serde(default = "default_status_added")]
    pub added: String,

    /// Colour for modified files
    #[serde(default = "default_status_modified")]
    pub modified: String,

    /// Colour for deleted files
    #[serde(default = "default_status_deleted")]
    pub deleted: String,

    /// Colour for untracked files
    #[serde(default = "default_status_untracked")]
    pub untracked: String,

    /// Colour for current branch
    #[serde(default = "default_status_current")]
    pub current: String,

    /// Colour for local branches
    #[serde(default = "default_status_local")]
    pub local: String,

    /// Colour for remote branches
    #[serde(default = "default_status_remote")]
    pub remote: String,
}

impl Default for StatusColours {
    fn default() -> Self {
        Self {
            added: "green".into(),
            modified: "cyan".into(),
            deleted: "red bold".into(),
            untracked: "cyan".into(),
            current: "cyan bold".into(),
            local: "cyan".into(),
            remote: "cyan".into(),
        }
    }
}

/// File type colours using LS_COLORS-style patterns
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileColours {
    /// Directory colour
    #[serde(default = "default_file_directory")]
    pub directory: String,

    /// Symbolic link colour
    #[serde(default = "default_file_symlink")]
    pub symlink: String,

    /// Executable file colour
    #[serde(default = "default_file_executable")]
    pub executable: String,

    /// Regular file colour
    #[serde(default = "default_file_regular")]
    pub regular: String,

    /// Custom colours for file extensions
    #[serde(default)]
    pub extensions: hashbrown::HashMap<String, String>,
}

impl Default for FileColours {
    fn default() -> Self {
        let mut extensions = hashbrown::HashMap::new();
        extensions.insert("rs".into(), "cyan".into());
        extensions.insert("js".into(), "cyan".into());
        extensions.insert("ts".into(), "cyan".into());
        extensions.insert("py".into(), "green".into());
        extensions.insert("toml".into(), "cyan".into());
        extensions.insert("md".into(), String::new());

        Self {
            directory: "bold cyan".into(),
            symlink: "cyan".into(),
            executable: "bold green".into(),
            regular: String::new(),
            extensions,
        }
    }
}

// Default value functions
macro_rules! serde_default_string {
    ($name:ident, $value:expr) => {
        fn $name() -> String {
            $value.into()
        }
    };
}

serde_default_string!(default_cli_success, "green");
serde_default_string!(default_cli_error, "red");
serde_default_string!(default_cli_warning, "red");
serde_default_string!(default_cli_info, "cyan");
serde_default_string!(default_cli_prompt, "bold cyan");

serde_default_string!(default_diff_new, "green");
serde_default_string!(default_diff_old, "red");
serde_default_string!(default_diff_context, "dim");
serde_default_string!(default_diff_header, "bold cyan");
serde_default_string!(default_diff_meta, "cyan");
serde_default_string!(default_diff_frag, "cyan");

serde_default_string!(default_status_added, "green");
serde_default_string!(default_status_modified, "cyan");
serde_default_string!(default_status_deleted, "red bold");
serde_default_string!(default_status_untracked, "cyan");
serde_default_string!(default_status_current, "cyan bold");
serde_default_string!(default_status_local, "cyan");
serde_default_string!(default_status_remote, "cyan");

serde_default_string!(default_file_directory, "bold cyan");
serde_default_string!(default_file_symlink, "cyan");
serde_default_string!(default_file_executable, "bold green");
serde_default_string!(default_file_regular, "");

impl ThemeConfig {
    /// Convert CLI colours to anstyle::Style
    pub fn parse_cli_styles(&self) -> Result<ParsedCliColours> {
        let parser = CachedStyleParser::default();
        Ok(ParsedCliColours {
            success: parser.parse_flexible(&self.cli.success)?,
            error: parser.parse_flexible(&self.cli.error)?,
            warning: parser.parse_flexible(&self.cli.warning)?,
            info: parser.parse_flexible(&self.cli.info)?,
            prompt: parser.parse_flexible(&self.cli.prompt)?,
        })
    }

    /// Convert diff colours to anstyle::Style
    pub fn parse_diff_styles(&self) -> Result<ParsedDiffColours> {
        let parser = CachedStyleParser::default();
        Ok(ParsedDiffColours {
            new: parser.parse_flexible(&self.diff.new)?,
            old: parser.parse_flexible(&self.diff.old)?,
            context: parser.parse_flexible(&self.diff.context)?,
            header: parser.parse_flexible(&self.diff.header)?,
            meta: parser.parse_flexible(&self.diff.meta)?,
            frag: parser.parse_flexible(&self.diff.frag)?,
        })
    }

    /// Convert status colours to anstyle::Style
    pub fn parse_status_styles(&self) -> Result<ParsedStatusColours> {
        let parser = CachedStyleParser::default();
        Ok(ParsedStatusColours {
            added: parser.parse_flexible(&self.status.added)?,
            modified: parser.parse_flexible(&self.status.modified)?,
            deleted: parser.parse_flexible(&self.status.deleted)?,
            untracked: parser.parse_flexible(&self.status.untracked)?,
            current: parser.parse_flexible(&self.status.current)?,
            local: parser.parse_flexible(&self.status.local)?,
            remote: parser.parse_flexible(&self.status.remote)?,
        })
    }

    /// Convert file colours to anstyle::Style
    pub fn parse_file_styles(&self) -> Result<ParsedFileColours> {
        let parser = CachedStyleParser::default();
        let mut extension_styles = hashbrown::HashMap::new();
        for (ext, colour_str) in &self.files.extensions {
            let style = parser
                .parse_flexible(colour_str)
                .with_context(|| format!("Failed to parse style for extension '{ext}': {colour_str}"))?;
            extension_styles.insert(ext.clone(), style);
        }

        Ok(ParsedFileColours {
            directory: parser.parse_flexible(&self.files.directory)?,
            symlink: parser.parse_flexible(&self.files.symlink)?,
            executable: parser.parse_flexible(&self.files.executable)?,
            regular: parser.parse_flexible(&self.files.regular)?,
            extensions: extension_styles,
        })
    }
}

/// Parsed CLI colours with [`AnsiStyle`] values ready for rendering.
#[derive(Debug, Clone)]
pub struct ParsedCliColours {
    /// Style for success messages.
    pub success: AnsiStyle,
    /// Style for error messages.
    pub error: AnsiStyle,
    /// Style for warning messages.
    pub warning: AnsiStyle,
    /// Style for info messages.
    pub info: AnsiStyle,
    /// Style for prompt text.
    pub prompt: AnsiStyle,
}

/// Parsed diff colours with [`AnsiStyle`] values ready for rendering.
#[derive(Debug, Clone)]
pub struct ParsedDiffColours {
    /// Style for added lines.
    pub new: AnsiStyle,
    /// Style for removed lines.
    pub old: AnsiStyle,
    /// Style for context (unchanged) lines.
    pub context: AnsiStyle,
    /// Style for diff headers.
    pub header: AnsiStyle,
    /// Style for diff metadata.
    pub meta: AnsiStyle,
    /// Style for fragment indicators.
    pub frag: AnsiStyle,
}

/// Parsed status colours with [`AnsiStyle`] values ready for rendering.
#[derive(Debug, Clone)]
pub struct ParsedStatusColours {
    /// Style for added files.
    pub added: AnsiStyle,
    /// Style for modified files.
    pub modified: AnsiStyle,
    /// Style for deleted files.
    pub deleted: AnsiStyle,
    /// Style for untracked files.
    pub untracked: AnsiStyle,
    /// Style for the current branch.
    pub current: AnsiStyle,
    /// Style for local branches.
    pub local: AnsiStyle,
    /// Style for remote branches.
    pub remote: AnsiStyle,
}

/// Parsed file-type colours with [`AnsiStyle`] values ready for rendering.
#[derive(Debug, Clone)]
pub struct ParsedFileColours {
    /// Style for directories.
    pub directory: AnsiStyle,
    /// Style for symbolic links.
    pub symlink: AnsiStyle,
    /// Style for executable files.
    pub executable: AnsiStyle,
    /// Style for regular files without a matching extension rule.
    pub regular: AnsiStyle,
    /// Per-extension styles keyed by extension (without the leading dot).
    pub extensions: hashbrown::HashMap<String, AnsiStyle>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ThemeConfig::default();
        assert_eq!(config.cli.success, "green");
        assert_eq!(config.diff.new, "green");
        assert_eq!(config.status.added, "green");
        assert_eq!(config.files.directory, "bold cyan");
    }

    #[test]
    fn test_load_from_toml() {
        let toml_content = r#"
[cli]
success = "bold green"
error = "bold red"

[diff]
new = "green"
old = "red"

[status]
added = "green"
modified = "cyan"

[files]
directory = "bold cyan"
executable = "bold cyan"

[files.extensions]
"rs" = "bright cyan"
"py" = "bright cyan"
"#;

        let temp_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(&temp_file, toml_content).unwrap();

        let config = ThemeConfig::load_from_file(&temp_file).expect("Failed to load config");
        assert_eq!(config.cli.success, "bold green");
        assert_eq!(config.diff.new, "green");
        assert_eq!(config.files.extensions.get("rs"), Some(&"bright cyan".to_owned()));
        assert_eq!(config.files.extensions.get("py"), Some(&"bright cyan".to_owned()));
    }

    #[test]
    fn test_parse_styles() {
        let config = ThemeConfig::default();

        let cli_styles = config.parse_cli_styles().expect("Failed to parse CLI styles");
        assert_ne!(cli_styles.success, AnsiStyle::new());

        let diff_styles = config.parse_diff_styles().expect("Failed to parse diff styles");
        assert_ne!(diff_styles.new, AnsiStyle::new());

        let status_styles = config.parse_status_styles().expect("Failed to parse status styles");
        assert_ne!(status_styles.added, AnsiStyle::new());

        let file_styles = config.parse_file_styles().expect("Failed to parse file styles");
        assert_ne!(file_styles.directory, AnsiStyle::new());
    }

    #[test]
    fn test_parse_custom_styles() {
        let mut config = ThemeConfig::default();
        config.cli.success = "bold red ul".to_owned();
        config.diff.new = "#00ff00".to_owned(); // RGB green
        config.files.symlink = "01;35".to_owned(); // ANSI code for bold magenta

        let cli_styles = config.parse_cli_styles().expect("Failed to parse CLI styles");
        assert!(cli_styles.success.get_effects().contains(anstyle::Effects::BOLD));
        assert!(cli_styles.success.get_effects().contains(anstyle::Effects::UNDERLINE));

        let diff_styles = config.parse_diff_styles().expect("Failed to parse diff styles");
        // The green colour should be set
        assert_ne!(diff_styles.new.get_fg_color(), None);

        let file_styles = config.parse_file_styles().expect("Failed to parse file styles");
        assert!(file_styles.symlink.get_effects().contains(anstyle::Effects::BOLD));
    }
}
