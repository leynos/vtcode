//! Safe atomic configuration file writing with smart merging.
//!
//! Provides atomic writes and marker-based config merging to preserve user customizations.

use crate::utils::file_utils::ensure_dir_exists_sync;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

/// Configuration file format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// Plain text or shell-style config (# comments)
    PlainText,
    /// TOML format
    Toml,
    /// JSON format
    Json,
    /// YAML format
    Yaml,
    /// JavaScript format (for .hyper.js, etc.)
    JavaScript,
}

/// Opening marker for a Diapason-managed configuration section.
pub const DIAPASON_BEGIN_MARKER: &str = "BEGIN DIAPASON CONFIGURATION";
/// Closing marker for a Diapason-managed configuration section.
pub const DIAPASON_END_MARKER: &str = "END DIAPASON CONFIGURATION";

/// Safe atomic configuration file writer
pub struct ConfigWriter;

impl ConfigWriter {
    /// Write content to a file atomically using a temp file and rename
    ///
    /// This ensures that the file is never left in a partially-written state
    pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            ensure_dir_exists_sync(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        // Create temp file in the same directory for atomic rename
        let temp_file = NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
            .with_context(|| format!("Failed to create temp file in directory: {}", path.display()))?;

        // Write content to temp file
        temp_file
            .as_file()
            .write_all(content.as_bytes())
            .with_context(|| "Failed to write content to temp file")?;

        // Sync to ensure data is written to disk
        temp_file
            .as_file()
            .sync_all()
            .with_context(|| "Failed to sync temp file to disk")?;

        // Atomically rename temp file to target path
        temp_file
            .persist(path)
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;

        Ok(())
    }

    /// Merge new Diapason configuration section with existing config
    ///
    /// Removes old Diapason sections and adds the new section, preserving user customizations
    pub fn merge_with_markers(existing: &str, new_section: &str, format: ConfigFormat) -> Result<String> {
        // Remove any existing Diapason section
        let cleaned = Self::remove_diapason_section(existing);

        // Add new Diapason section with markers
        let diapason_section = Self::wrap_with_markers(new_section, format);

        // Determine where to insert the new section
        let merged = if cleaned.trim().is_empty() {
            // File is empty, just use the Diapason section
            diapason_section
        } else {
            // Prepend Diapason section so user-defined settings stay authoritative.
            // Most terminal configs resolve duplicate keys as "last value wins".
            format!("{}\n\n{}", diapason_section, cleaned.trim_start())
        };

        Ok(merged)
    }

    /// Remove existing Diapason configuration section from content
    fn remove_diapason_section(content: &str) -> String {
        let mut result = Vec::new();
        let mut in_diapason_section = false;

        for line in content.lines() {
            if line.contains(DIAPASON_BEGIN_MARKER) {
                in_diapason_section = true;
                continue;
            }

            if line.contains(DIAPASON_END_MARKER) {
                in_diapason_section = false;
                continue;
            }

            if !in_diapason_section {
                result.push(line);
            }
        }

        result.join("\n")
    }

    /// Wrap configuration content with Diapason markers
    fn wrap_with_markers(content: &str, format: ConfigFormat) -> String {
        let comment_prefix = match format {
            ConfigFormat::PlainText | ConfigFormat::Toml | ConfigFormat::Yaml => "#",
            ConfigFormat::Json => "//", // JSON doesn't support comments, but some parsers allow them
            ConfigFormat::JavaScript => "//",
        };

        let header = format!(
            "{comment_prefix} {DIAPASON_BEGIN_MARKER}\n{comment_prefix} Diapason-managed section - auto-generated\n{comment_prefix} Do not edit manually"
        );

        let footer = format!("{comment_prefix} {DIAPASON_END_MARKER}");

        format!("{}\n{}\n{}", header, content.trim(), footer)
    }

    /// Detect config file format from extension
    pub fn detect_format(path: &Path) -> ConfigFormat {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            match ext {
                "toml" => ConfigFormat::Toml,
                "json" => ConfigFormat::Json,
                "yaml" | "yml" => ConfigFormat::Yaml,
                "js" => ConfigFormat::JavaScript,
                _ => ConfigFormat::PlainText,
            }
        } else {
            ConfigFormat::PlainText
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_atomic_write() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test.conf");

        let content = "test content";
        ConfigWriter::write_atomic(&path, content).unwrap();

        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn test_remove_diapason_section() {
        let content = r#"# User config
user_setting = 1

# BEGIN DIAPASON CONFIGURATION
# Diapason-managed section - auto-generated
diapason_setting = 2
# END DIAPASON CONFIGURATION

# More user config
another_setting = 3
"#;

        let result = ConfigWriter::remove_diapason_section(content);

        assert!(!result.contains("diapason_setting"));
        assert!(result.contains("user_setting"));
        assert!(result.contains("another_setting"));
        assert!(!result.contains("DIAPASON CONFIGURATION"));
    }

    #[test]
    fn test_merge_with_markers() {
        let existing = r#"# User config
user_setting = 1
"#;

        let new_section = "diapason_setting = 2";

        let result = ConfigWriter::merge_with_markers(existing, new_section, ConfigFormat::PlainText).unwrap();

        assert!(result.contains("user_setting"));
        assert!(result.contains("diapason_setting"));
        assert!(result.contains(DIAPASON_BEGIN_MARKER));
        assert!(result.contains(DIAPASON_END_MARKER));
        assert!(
            result.find("diapason_setting").expect("missing Diapason setting")
                < result.find("user_setting").expect("missing user setting")
        );
    }

    #[test]
    fn test_merge_empty_file() {
        let new_section = "diapason_setting = 1";

        let result = ConfigWriter::merge_with_markers("", new_section, ConfigFormat::PlainText).unwrap();

        assert!(result.contains("diapason_setting"));
        assert!(result.contains(DIAPASON_BEGIN_MARKER));
    }

    #[test]
    fn test_detect_format() {
        assert_eq!(ConfigWriter::detect_format(Path::new("test.toml")), ConfigFormat::Toml);
        assert_eq!(ConfigWriter::detect_format(Path::new("test.json")), ConfigFormat::Json);
        assert_eq!(ConfigWriter::detect_format(Path::new("test.yaml")), ConfigFormat::Yaml);
        assert_eq!(ConfigWriter::detect_format(Path::new("test.js")), ConfigFormat::JavaScript);
        assert_eq!(ConfigWriter::detect_format(Path::new("test.conf")), ConfigFormat::PlainText);
    }

    #[test]
    fn test_wrap_with_markers_toml() {
        let content = "setting = 1";
        let result = ConfigWriter::wrap_with_markers(content, ConfigFormat::Toml);

        assert!(result.starts_with("# BEGIN DIAPASON CONFIGURATION"));
        assert!(result.ends_with("# END DIAPASON CONFIGURATION"));
        assert!(result.contains("setting = 1"));
    }

    #[test]
    fn test_wrap_with_markers_javascript() {
        let content = "const setting = 1;";
        let result = ConfigWriter::wrap_with_markers(content, ConfigFormat::JavaScript);

        assert!(result.starts_with("// BEGIN DIAPASON CONFIGURATION"));
        assert!(result.contains("const setting = 1;"));
    }
}
