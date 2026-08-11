//! Safety / audit configuration.
//!
//! This module wires the `vtcode_safety::audit_log` sinks into the top-level
//! VT Code configuration. The struct is purely additive — existing configs
//! that don't declare a `[safety.audit]` table get the defaults from
//! [`ToolAuditConfig::default`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Configuration block for the persistent tool-call audit log.
///
/// ```toml
/// [safety.audit]
/// enabled = true
/// path = "~/.vtcode/audit/tools.jsonl"
/// max_size_bytes = 32 * 1024 * 1024
/// max_files = 4
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// Audit log configuration. `None` ⇒ audit disabled, equivalent to a
    /// disabled [`ToolAuditConfig`].
    #[serde(default)]
    audit: Option<ToolAuditConfig>,
}

/// Subset of `[safety.audit]` controlling the audit log sink.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAuditConfig {
    /// When true, the runloop persists audit entries. Default `false`.
    #[serde(default)]
    enabled: bool,
    /// Path of the JSONL file. Tilde expansion is performed at load time.
    #[serde(default = "default_audit_path")]
    path: PathBuf,
    /// Rotation threshold in bytes. Default 32 MiB.
    #[serde(default = "default_max_size_bytes")]
    max_size_bytes: u64,
    /// Maximum number of rotated files to keep (current file + N-1 archives).
    /// Default `4`.
    #[serde(default = "default_max_files")]
    max_files: usize,
}

impl ToolAuditConfig {
    /// Whether this audit sink should be constructed.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Configured JSONL path, before any environment-specific expansion.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Maximum size of the active JSONL file before rotation.
    #[must_use]
    pub fn max_size_bytes(&self) -> u64 {
        self.max_size_bytes
    }

    /// Maximum number of JSONL files retained, including the active file.
    #[must_use]
    pub fn max_files(&self) -> usize {
        self.max_files
    }
}

impl Default for ToolAuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_audit_path(),
            max_size_bytes: default_max_size_bytes(),
            max_files: default_max_files(),
        }
    }
}

fn default_audit_path() -> PathBuf {
    PathBuf::from("~/.vtcode/audit/tools.jsonl")
}

fn default_max_size_bytes() -> u64 {
    32 * 1024 * 1024
}

fn default_max_files() -> usize {
    4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_disable_audit() {
        let cfg = ToolAuditConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.max_files, 4);
        assert_eq!(cfg.max_size_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn safety_config_defaults_to_no_audit() {
        let cfg = SafetyConfig::default();
        assert!(cfg.audit.is_none());
    }

    #[test]
    fn parses_minimal_toml() {
        let parsed: ToolAuditConfig = toml::from_str(
            r#"
            enabled = true
            path = "/tmp/audit.jsonl"
            "#,
        )
        .expect("minimal toml");
        assert!(parsed.enabled);
        assert_eq!(parsed.path, PathBuf::from("/tmp/audit.jsonl"));
        // Defaults apply to omitted fields.
        assert_eq!(parsed.max_files, 4);
        assert_eq!(parsed.max_size_bytes, 32 * 1024 * 1024);
    }
}
