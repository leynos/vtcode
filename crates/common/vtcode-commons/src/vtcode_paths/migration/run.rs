//! Migration orchestration and completion-marker decisions.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Result, anyhow};

use super::super::ensure_private_dir;
use super::{
    MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason, VtCodePaths,
    copy_legacy_tree_with_exclusions, legacy_mappings, persist_report, persist_report_best_effort,
    record_unmapped_entries, valid_migration_marker, write_private_atomic,
};

/// Migrates eligible legacy global-path content into canonical locations.
#[derive(Debug, Clone)]
pub struct LegacyMigrator {
    paths: VtCodePaths,
}

impl LegacyMigrator {
    /// Creates a migrator bound to one resolved global path policy.
    pub fn new(paths: VtCodePaths) -> Self {
        Self { paths }
    }

    /// Scans and copies eligible legacy content without modifying its source.
    ///
    /// # Errors
    /// Returns an error only when the migration marker has no parent directory.
    pub fn run(&self) -> Result<MigrationReport> {
        let marker = self.paths.migration_marker_path();
        let mut report = MigrationReport::default();
        let marker_parent = marker.parent().ok_or_else(|| anyhow!("migration marker has no parent"))?;
        if let Err(error) = ensure_private_dir(marker_parent) {
            report.failures.push(MigrationFailure {
                path: marker_parent.to_path_buf(),
                error: error.to_string(),
            });
            persist_report_best_effort(&self.paths, &report);
            return Ok(report);
        }
        let marker_blocked = match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                report.failures.push(MigrationFailure {
                    path: marker.clone(),
                    error: "migration marker is a symlink; refusing to follow it".to_string(),
                });
                true
            }
            Ok(metadata) if metadata.is_file() => match valid_migration_marker(&marker) {
                Ok(true) => {
                    return Ok(MigrationReport {
                        already_completed: true,
                        ..MigrationReport::default()
                    });
                }
                Ok(false) => {
                    report.failures.push(MigrationFailure {
                        path: marker.clone(),
                        error: "migration marker has invalid contents or permissions".to_string(),
                    });
                    true
                }
                Err(error) => {
                    report.failures.push(MigrationFailure {
                        path: marker.clone(),
                        error: format!("could not validate migration marker: {error}"),
                    });
                    true
                }
            },
            Ok(_) => {
                report.failures.push(MigrationFailure {
                    path: marker.clone(),
                    error: "migration marker is not a regular file".to_string(),
                });
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                report.failures.push(MigrationFailure {
                    path: marker.clone(),
                    error: format!("could not inspect migration marker: {error}"),
                });
                true
            }
        };

        let legacy_root = self.paths.legacy_home_dir();
        let legacy_root_is_safe = match fs::symlink_metadata(legacy_root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                report.failures.push(MigrationFailure {
                    path: legacy_root.to_path_buf(),
                    error: "legacy root is a symlink; refusing to traverse it".to_string(),
                });
                false
            }
            Ok(metadata) if !metadata.is_dir() => {
                report.failures.push(MigrationFailure {
                    path: legacy_root.to_path_buf(),
                    error: "legacy root is not a directory".to_string(),
                });
                false
            }
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => {
                report.failures.push(MigrationFailure {
                    path: legacy_root.to_path_buf(),
                    error: format!("could not inspect legacy root: {error}"),
                });
                false
            }
        };

        if legacy_root_is_safe {
            let mappings = legacy_mappings(&self.paths);
            for mapping in &mappings {
                if mapping.source == mapping.destination {
                    continue;
                }
                if mapping.skip {
                    if fs::symlink_metadata(&mapping.source).is_ok() {
                        report.skipped.push(MigrationSkip {
                            path: mapping.source.clone(),
                            reason: MigrationSkipReason::Excluded,
                        });
                    }
                    continue;
                }
                if let Err(error) = copy_legacy_tree_with_exclusions(
                    &mapping.source,
                    &mapping.destination,
                    &mut report,
                    mapping.excluded_children,
                ) {
                    report.failures.push(MigrationFailure {
                        path: mapping.source.clone(),
                        error: error.to_string(),
                    });
                }
            }
            record_unmapped_entries(legacy_root, &mappings, &mut report);
        }

        if report.has_retryable_failures() || marker_blocked {
            persist_report_best_effort(&self.paths, &report);
            return Ok(report);
        }

        // Persist the complete scan report before publishing the completion
        // marker. If diagnostics cannot be recorded, retry the migration
        // rather than claiming a successful one-time migration.
        if let Err(error) = persist_report(&self.paths, &report) {
            report.failures.push(MigrationFailure {
                path: self.paths.migration_report_path(),
                error: error.to_string(),
            });
            persist_report_best_effort(&self.paths, &report);
            return Ok(report);
        }

        if let Err(error) = write_private_atomic(&marker, b"legacy migration completed\n") {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io_error| io_error.kind() == io::ErrorKind::AlreadyExists)
            {
                if valid_migration_marker(&marker).unwrap_or(false) {
                    report.already_completed = true;
                } else {
                    report.failures.push(MigrationFailure {
                        path: marker,
                        error: "migration marker appeared but is invalid".to_string(),
                    });
                }
                return Ok(report);
            }
            report
                .failures
                .push(MigrationFailure { path: marker, error: error.to_string() });
            persist_report_best_effort(&self.paths, &report);
            return Ok(report);
        }
        report.marker_written = true;
        if let Err(error) = persist_report(&self.paths, &report) {
            report.failures.push(MigrationFailure {
                path: self.paths.migration_report_path(),
                error: error.to_string(),
            });
        }
        Ok(report)
    }
}
