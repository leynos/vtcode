//! Public migration report records and private mapping metadata.

use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};

/// Serializes a filesystem path as a lossy UTF-8 string.
///
/// Serde's `PathBuf` serializer rejects paths that are not valid UTF-8, which
/// would make a migration report containing such a legacy path impossible to
/// persist. Rendering the path lossily keeps the report writable while the
/// JSON shape remains a plain string.
fn serialize_path_lossily<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
}

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
    #[serde(serialize_with = "serialize_path_lossily")]
    pub source: PathBuf,
    #[serde(serialize_with = "serialize_path_lossily")]
    pub destination: PathBuf,
}

/// A legacy item that was deliberately left untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationSkip {
    #[serde(serialize_with = "serialize_path_lossily")]
    pub path: PathBuf,
    pub reason: MigrationSkipReason,
}

/// An individual migration failure; remaining entries are still scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationFailure {
    #[serde(serialize_with = "serialize_path_lossily")]
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn migration_report_serializes_non_utf8_paths_lossily() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        use super::{MigrationEntry, MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason};

        let path = PathBuf::from(OsStr::from_bytes(b"legacy-\xff-name"));
        assert!(path.to_str().is_none(), "fixture path must not be valid UTF-8");
        let lossy = path.to_string_lossy().into_owned();
        assert_eq!(lossy, "legacy-\u{fffd}-name");

        let report = MigrationReport {
            migrated: vec![MigrationEntry { source: path.clone(), destination: path.clone() }],
            skipped: vec![MigrationSkip {
                path: path.clone(),
                reason: MigrationSkipReason::Symlink,
            }],
            failures: vec![MigrationFailure { path, error: "unreadable legacy entry".to_string() }],
            marker_written: false,
            already_completed: false,
        };

        let serialized = serde_json::to_value(&report).expect("serialize migration report");

        assert_eq!(
            serialized,
            serde_json::json!({
                "migrated": [{"source": lossy.as_str(), "destination": lossy.as_str()}],
                "skipped": [{"path": lossy.as_str(), "reason": "symlink"}],
                "failures": [{"path": lossy.as_str(), "error": "unreadable legacy entry"}],
                "marker_written": false,
                "already_completed": false,
            })
        );
    }
}
