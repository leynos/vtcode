//! Audit logging for externally modified file conflicts.

use crate::utils::error_messages::ERR_CREATE_AUDIT_DIR;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Local};
use diapason_commons::DiapasonPaths;
use serde::{Deserialize, Serialize};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// Audit event emitted when a tracked file changes outside Diapason control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConflictAuditEvent {
    pub timestamp: DateTime<Local>,
    pub path: PathBuf,
    pub reason: String,
    pub file_exists: bool,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

/// JSONL audit logger for file-conflict events.
pub struct FileConflictAuditLog {
    writer: Option<BufWriter<std::fs::File>>,
    log_path: PathBuf,
}

impl FileConflictAuditLog {
    /// Create or open today's file-conflict audit log in the given directory.
    pub fn new(audit_dir: PathBuf) -> Result<Self> {
        ensure!(!audit_dir.as_os_str().is_empty(), "File conflict audit directory is unavailable");
        DiapasonPaths::ensure_user_dir(&audit_dir).context(ERR_CREATE_AUDIT_DIR)?;

        let date = Local::now().format("%Y-%m-%d");
        let log_path = audit_dir.join(format!("file-conflicts-{date}.log"));
        Ok(Self { writer: None, log_path })
    }

    pub fn record(&mut self, event: &FileConflictAuditEvent) -> Result<()> {
        use std::io::Write;

        let json = serde_json::to_string(event).context("Failed to serialize file conflict audit event")?;
        let writer = self.writer_mut()?;
        writeln!(writer, "{json}").context("Failed to write file conflict audit event")?;
        writer.flush().context("Failed to flush file conflict audit log")?;
        Ok(())
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    fn writer_mut(&mut self) -> Result<&mut BufWriter<std::fs::File>> {
        if self.writer.is_none() {
            let file = DiapasonPaths::open_private_append_file(&self.log_path)
                .with_context(|| format!("Failed to open file conflict audit log at {:?}", self.log_path))?;
            self.writer = Some(BufWriter::new(file));
        }

        self.writer
            .as_mut()
            .context("file conflict audit log writer was not initialized")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn unavailable_audit_directory_is_rejected_before_filesystem_access() -> Result<()> {
        let error = FileConflictAuditLog::new(PathBuf::new())
            .err()
            .context("an unavailable audit directory must fail")?;
        assert_eq!(error.to_string(), "File conflict audit directory is unavailable");
        Ok(())
    }

    #[test]
    fn defers_file_conflict_audit_log_creation_until_first_record() -> Result<()> {
        let dir = TempDir::new()?;
        let log = FileConflictAuditLog::new(dir.path().to_path_buf())?;
        assert!(!log.log_path().exists());
        Ok(())
    }
}
