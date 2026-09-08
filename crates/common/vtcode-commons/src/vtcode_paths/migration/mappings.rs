//! Legacy layout mapping and unmapped-entry reporting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{LegacyMapping, MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason, VtCodePaths};

pub(super) fn legacy_mappings(paths: &VtCodePaths) -> Vec<LegacyMapping> {
    let legacy = paths.legacy_home_dir();
    let mut mappings = Vec::with_capacity(50);
    for name in [
        "vtcode.toml",
        "update.toml",
        "config.toml",
        "AGENTS.md",
        "AGENTS.override.md",
        "CLAUDE.md",
        "commands",
        "agents",
        "rules",
        "prompts",
        "tool-policy.json",
        "mcp.json",
        "mcp.toml",
        "mcp-config.json",
        "mcp-config.toml",
        "mcp",
        "auth",
        "output-styles",
        "output_styles",
    ] {
        add_mapping(&mut mappings, legacy, name, paths.config_dir().join(name));
    }
    add_mapping(&mut mappings, legacy, "auth.json", paths.auth_file());
    for name in [
        "plugins",
        "skills",
        "installed-skills",
        "assets",
        "durable-assets",
        "tools",
    ] {
        add_mapping(&mut mappings, legacy, name, paths.data_dir().join(name));
    }
    add_mapping(&mut mappings, legacy, "bin", paths.executable_dir().to_path_buf());
    for name in [
        "projects",
        "sessions",
        "history",
        "memory",
        "agent-memory",
        "audit",
        "logs",
        "scheduler",
        "pods",
        "checkpoints",
        "backups",
    ] {
        add_mapping(&mut mappings, legacy, name, paths.state_dir().join(name));
    }
    add_mapping(
        &mut mappings,
        legacy,
        "ast_grep_install_cache.json",
        paths.cache_dir().join("ast-grep/install.json"),
    );
    add_mapping(
        &mut mappings,
        legacy,
        "ripgrep_install_cache.json",
        paths.cache_dir().join("ripgrep/ripgrep_install_cache.json"),
    );
    // Before the centralized path policy, DotManager kept cache, logs,
    // sessions, and backups directly below the configuration directory. On
    // native platforms that directory is still the current config root, so
    // those files are outside the legacy-home scan above and need explicit
    // compatibility mappings.
    for (name, destination) in [
        ("cache", paths.cache_dir().to_path_buf()),
        ("logs", paths.state_dir().join("logs")),
        ("sessions", paths.state_dir().join("sessions")),
        ("backups", paths.state_dir().join("backups")),
    ] {
        let source = paths.config_dir().join(name);
        if source != legacy.join(name) {
            mappings.push(LegacyMapping {
                source,
                destination,
                skip: false,
                excluded_children: &[],
            });
        }
    }

    for name in [
        "model-cache",
        "prompt-cache",
        "approval-data",
        "ast-grep",
        "ast-grep.lock",
        "web-fetch",
        "large-output",
    ] {
        add_mapping(&mut mappings, legacy, name, paths.cache_dir().join(name));
    }
    // Keep the pre-XDG configuration root ahead of the legacy home cache when
    // both layouts exist; it is the most recent location used by DotManager.
    add_mapping(&mut mappings, legacy, "cache", paths.cache_dir().to_path_buf());
    add_mapping(&mut mappings, legacy, ".cache", paths.cache_dir().to_path_buf());

    mappings.push(LegacyMapping {
        source: legacy.join("state"),
        destination: paths.state_dir().to_path_buf(),
        skip: false,
        // Migration metadata is owned by this protocol. Copying a legacy
        // marker could make an incomplete scan look completed.
        excluded_children: &["migration"],
    });
    mappings.push(LegacyMapping {
        source: legacy.join("tmp"),
        destination: paths.runtime_dir().join("tmp"),
        skip: true,
        excluded_children: &[],
    });
    mappings
}

fn add_mapping(mappings: &mut Vec<LegacyMapping>, legacy: &Path, name: &str, destination: PathBuf) {
    mappings.push(LegacyMapping {
        source: legacy.join(name),
        destination,
        skip: false,
        excluded_children: &[],
    });
}

pub(super) fn record_unmapped_entries(legacy_root: &Path, mappings: &[LegacyMapping], report: &mut MigrationReport) {
    let entries = match fs::read_dir(legacy_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            report.failures.push(MigrationFailure {
                path: legacy_root.to_path_buf(),
                error: format!("could not list legacy root: {error}"),
            });
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.failures.push(MigrationFailure {
                    path: legacy_root.to_path_buf(),
                    error: format!("could not inspect legacy entry: {error}"),
                });
                continue;
            }
        };
        if !mappings.iter().any(|mapping| mapping.source == entry.path()) {
            report.skipped.push(MigrationSkip {
                path: entry.path(),
                reason: MigrationSkipReason::Unmapped,
            });
        }
    }
}
