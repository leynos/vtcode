//! Public migration report records and private mapping metadata.

use std::path::PathBuf;

use serde::Serialize;

/// Outcome of a legacy migration attempt.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    /// Files copied by the migration.
    pub migrated: Vec<MigrationEntry>,
    /// Legacy items deliberately left untouched.
    pub skipped: Vec<MigrationSkip>,
    /// Individual failures encountered during migration.
    pub failures: Vec<MigrationFailure>,
    /// Whether the migration-complete marker was written.
    pub marker_written: bool,
    /// Whether a previous run already completed the migration.
    pub already_completed: bool,
}

impl MigrationReport {
    /// Returns whether a later startup should retry the migration.
    pub fn has_retryable_failures(&self) -> bool {
        !self.failures.is_empty()
    }
}

/// A source file copied by migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationEntry {
    /// Path the file was copied from.
    pub source: PathBuf,
    /// Path the file was copied to.
    pub destination: PathBuf,
}

/// A legacy item that was deliberately left untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationSkip {
    /// Legacy path that was skipped.
    pub path: PathBuf,
    /// Why the path was skipped.
    pub reason: MigrationSkipReason,
}

/// An individual migration failure; remaining entries are still scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationFailure {
    /// Path that could not be migrated.
    pub path: PathBuf,
    /// Description of the failure.
    pub error: String,
}

/// Reason a legacy item was not copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationSkipReason {
    /// A file already exists at the destination.
    DestinationExists,
    /// The entry is a symbolic link.
    Symlink,
    /// The entry is not a regular file.
    SpecialFile,
    /// The entry is on the exclusion list.
    Excluded,
    /// No destination mapping exists for the entry.
    Unmapped,
}

pub(super) struct LegacyMapping {
    pub(super) source: PathBuf,
    pub(super) destination: PathBuf,
    pub(super) skip: bool,
    pub(super) excluded_children: &'static [&'static str],
}
