//! Retryable, symlink-safe migration from pre-XDG VT Code storage.

mod copy;
mod mappings;
mod model;
mod run;

pub use model::{MigrationEntry, MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason};
pub use run::LegacyMigrator;

use super::VtCodePaths;
use copy::{
    copy_legacy_tree_with_exclusions, persist_report, persist_report_best_effort, valid_migration_marker,
    write_private_atomic,
};
use mappings::{legacy_mappings, record_unmapped_entries};
use model::LegacyMapping;
