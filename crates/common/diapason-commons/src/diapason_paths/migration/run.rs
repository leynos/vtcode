//! Migration orchestration and completion-marker decisions.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Result, anyhow};

use super::super::ensure_private_dir;
use super::{
    DiapasonPaths, LegacyMapping, MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason,
    copy_legacy_tree_with_exclusions, legacy_mappings, persist_report, persist_report_best_effort,
    record_unmapped_entries, valid_migration_marker, write_private_atomic,
};

/// Migrates eligible legacy global-path content into canonical locations.
#[derive(Debug, Clone)]
pub struct LegacyMigrator {
    paths: DiapasonPaths,
}

/// Result of inspecting the migration marker before scanning legacy content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerState {
    Completed,
    Blocked,
    Absent,
}

// refused_by: merge_ladder_exhausted · collision: `if mapping.source == mapping.destination {` +24 more
/// Inspects the migration marker, recording any failure in `report`.
fn inspect_marker(marker: &Path, report: &mut MigrationReport) -> MarkerState {
    match fs::symlink_metadata(marker) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            report.failures.push(MigrationFailure {
                path: marker.to_path_buf(),
                error: "migration marker is a symlink; refusing to follow it".to_string(),
            });
            MarkerState::Blocked
        }
        Ok(metadata) if metadata.is_file() => match valid_migration_marker(marker) {
            Ok(true) => MarkerState::Completed,
            Ok(false) => {
                report.failures.push(MigrationFailure {
                    path: marker.to_path_buf(),
                    error: "migration marker has invalid contents or permissions".to_string(),
                });
                MarkerState::Blocked
            }
            Err(error) => {
                report.failures.push(MigrationFailure {
                    path: marker.to_path_buf(),
                    error: format!("could not validate migration marker: {error}"),
                });
                MarkerState::Blocked
            }
        },
        Ok(_) => {
            report.failures.push(MigrationFailure {
                path: marker.to_path_buf(),
                error: "migration marker is not a regular file".to_string(),
            });
            MarkerState::Blocked
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => MarkerState::Absent,
        Err(error) => {
            report.failures.push(MigrationFailure {
                path: marker.to_path_buf(),
                error: format!("could not inspect migration marker: {error}"),
            });
            MarkerState::Blocked
        }
    }
}

/// Returns whether the legacy root can be traversed, recording failures in `report`.
fn legacy_root_is_safe(legacy_root: &Path, report: &mut MigrationReport) -> bool {
    match fs::symlink_metadata(legacy_root) {
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
    }
}

impl LegacyMigrator {
    /// Creates a migrator bound to one resolved global path policy.
    pub fn new(paths: DiapasonPaths) -> Self {
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

        let marker_state = inspect_marker(&marker, &mut report);
        if matches!(marker_state, MarkerState::Completed) {
            return Ok(MigrationReport {
                already_completed: true,
                ..MigrationReport::default()
            });
        }
        let marker_blocked = matches!(marker_state, MarkerState::Blocked);

        let legacy_root = self.paths.legacy_home_dir();
        if legacy_root_is_safe(legacy_root, &mut report) {
            self.copy_mappings(legacy_root, &mut report);
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

        Ok(self.publish_completion(&marker, report))
    }

    /// Copies every legacy mapping into its canonical destination.
    fn copy_mappings(&self, legacy_root: &Path, report: &mut MigrationReport) {
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
                report,
                mapping.excluded_children,
            ) {
                report.failures.push(MigrationFailure {
                    path: mapping.source.clone(),
                    error: error.to_string(),
                });
            }
        }
        record_unmapped_entries(legacy_root, &mappings, report);
    }

    /// Publishes the completion marker, recording any failure in `report`.
    fn publish_completion(&self, marker: &Path, mut report: MigrationReport) -> MigrationReport {
        if let Err(error) = write_private_atomic(marker, b"legacy migration completed\n") {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io_error| io_error.kind() == io::ErrorKind::AlreadyExists)
            {
                if valid_migration_marker(marker).unwrap_or(false) {
                    report.already_completed = true;
                } else {
                    report.failures.push(MigrationFailure {
                        path: marker.to_path_buf(),
                        error: "migration marker appeared but is invalid".to_string(),
                    });
                }
                return report;
            }
            report.failures.push(MigrationFailure {
                path: marker.to_path_buf(),
                error: error.to_string(),
            });
            persist_report_best_effort(&self.paths, &report);
            return report;
        }

        report.marker_written = true;
        if let Err(error) = persist_report(&self.paths, &report) {
            report.failures.push(MigrationFailure {
                path: self.paths.migration_report_path(),
                error: error.to_string(),
            });
        }
        report
    }
}




// weave: run 'weave explain crates/common/diapason-commons/src/diapason_paths/migration/run.rs' for per-hunk detail, 'weave check' to verify your resolution
