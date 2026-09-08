//! Symlink-safe copying and durable migration report persistence.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};

use super::super::{create_private_new_file, ensure_migration_dir, ensure_private_dir, open_no_follow};
use super::{MigrationEntry, MigrationFailure, MigrationReport, MigrationSkip, MigrationSkipReason, VtCodePaths};

fn copy_legacy_tree(source: &Path, destination: &Path, report: &mut MigrationReport) -> Result<()> {
    copy_legacy_tree_with_exclusions(source, destination, report, &[])
}

pub(super) fn copy_legacy_tree_with_exclusions(
    source: &Path,
    destination: &Path,
    report: &mut MigrationReport,
    excluded_children: &[&str],
) -> Result<()> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("could not inspect legacy path {}", source.display())),
    };
    if metadata.file_type().is_symlink() {
        report.skipped.push(MigrationSkip {
            path: source.to_path_buf(),
            reason: MigrationSkipReason::Symlink,
        });
        return Ok(());
    }
    if metadata.is_file() {
        return copy_regular_file(source, destination, report);
    }
    if !metadata.is_dir() {
        report.skipped.push(MigrationSkip {
            path: source.to_path_buf(),
            reason: MigrationSkipReason::SpecialFile,
        });
        return Ok(());
    }
    ensure_migration_dir(destination)?;
    let entries =
        fs::read_dir(source).with_context(|| format!("could not read legacy directory {}", source.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.failures.push(MigrationFailure {
                    path: source.to_path_buf(),
                    error: format!("could not inspect legacy entry: {error}"),
                });
                continue;
            }
        };
        let child_source = entry.path();
        let child_destination = destination.join(entry.file_name());
        if excluded_children.iter().any(|name| entry.file_name() == OsStr::new(name)) {
            report.skipped.push(MigrationSkip {
                path: child_source,
                reason: MigrationSkipReason::Excluded,
            });
            continue;
        }
        if let Err(error) = copy_legacy_tree(&child_source, &child_destination, report) {
            report
                .failures
                .push(MigrationFailure { path: child_source, error: error.to_string() });
        }
    }
    Ok(())
}

fn copy_regular_file(source: &Path, destination: &Path, report: &mut MigrationReport) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        report.skipped.push(MigrationSkip {
            path: source.to_path_buf(),
            reason: if metadata.file_type().is_symlink() {
                MigrationSkipReason::Symlink
            } else {
                MigrationSkipReason::DestinationExists
            },
        });
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("migration destination {} has no parent", destination.display()))?;
    ensure_migration_dir(parent)?;
    let (temporary, mut output) = unique_private_file(parent, destination.file_name().unwrap_or(OsStr::new("file")))?;
    let mut input =
        open_no_follow(source).with_context(|| format!("could not safely open legacy file {}", source.display()))?;
    let result: Result<()> = (|| {
        let _bytes_copied = io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        match fs::hard_link(&temporary, destination) {
            Ok(()) => {
                fs::remove_file(&temporary)?;
                report.migrated.push(MigrationEntry {
                    source: source.to_path_buf(),
                    destination: destination.to_path_buf(),
                });
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                report.skipped.push(MigrationSkip {
                    path: source.to_path_buf(),
                    reason: MigrationSkipReason::DestinationExists,
                });
                Ok(())
            }
            Err(error) => Err(error).with_context(|| format!("could not atomically publish {}", destination.display())),
        }
    })();
    if result.is_err() {
        remove_temporary_file(&temporary);
    }
    result
}

pub(super) fn persist_report(paths: &VtCodePaths, report: &MigrationReport) -> Result<()> {
    let serialized = serde_json::to_vec_pretty(report).context("could not serialize legacy migration report")?;
    VtCodePaths::write_private_file_atomic(paths.migration_report_path(), &serialized)
        .with_context(|| format!("could not write migration report {}", paths.migration_report_path().display()))
}

pub(super) fn persist_report_best_effort(paths: &VtCodePaths, report: &MigrationReport) {
    if let Err(error) = persist_report(paths, report) {
        tracing::debug!(error = %error, "could not persist legacy migration report");
    }
}

pub(super) fn write_private_atomic(destination: &Path, contents: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("private file {} has no parent", destination.display()))?;
    ensure_private_dir(parent)?;
    let (temporary, mut file) = unique_private_file(parent, destination.file_name().unwrap_or(OsStr::new("file")))?;
    let result: io::Result<()> = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&temporary, destination)?;
        fs::remove_file(&temporary)?;
        Ok(())
    })();
    if result.is_err() {
        remove_temporary_file(&temporary);
    }
    result.with_context(|| format!("could not publish {}", destination.display()))
}

pub(super) fn valid_migration_marker(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Ok(false);
        }
    }
    let mut file = open_no_follow(path)?;
    let mut contents = Vec::new();
    let _bytes_read = file.read_to_end(&mut contents)?;
    Ok(contents == b"legacy migration completed\n")
}

fn unique_private_file(parent: &Path, stem: &OsStr) -> Result<(PathBuf, File)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let stem = stem.to_string_lossy();
    for attempt in 0..32u8 {
        let temporary = parent.join(format!(".{stem}.{}.{}.{}.migration", std::process::id(), timestamp, attempt));
        match create_private_new_file(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).with_context(|| format!("could not create {}", temporary.display())),
        }
    }
    bail!("could not allocate a unique migration temporary file in {}", parent.display())
}

fn remove_temporary_file(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(path = %path.display(), %error, "failed to remove temporary migration file");
    }
}
