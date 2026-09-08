//! Public migration report records and private mapping metadata.

use std::path::PathBuf;

use serde::Serialize;

/// Outcome of a legacy migration attempt.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    pub migrated: Vec<MigrationEntry>,
    pub skipped: Vec<MigrationSkip>,
    pub failures: Vec<MigrationFailure>,
    pub marker_written: bool,
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
    pub source: PathBuf,
    pub destination: PathBuf,
}

/// A legacy item that was deliberately left untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationSkip {
    pub path: PathBuf,
    pub reason: MigrationSkipReason,
}

/// An individual migration failure; remaining entries are still scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationFailure {
    pub path: PathBuf,
    pub error: String,
}

/// Reason a legacy item was not copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationSkipReason {
    DestinationExists,
    Symlink,
    SpecialFile,
    Excluded,
    Unmapped,
}

pub(super) struct LegacyMapping {
    pub(super) source: PathBuf,
    pub(super) destination: PathBuf,
    pub(super) skip: bool,
    pub(super) excluded_children: &'static [&'static str],
}
