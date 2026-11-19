//! Default [`ExecSessionOutputReader`] adapter over live exec sessions and the
//! session archive.
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use std::path::Path;

use crate::subagents::background::load_archive_transcript;
use crate::tools::exec_session::{ExecSessionManager, ExecSessionNotFound};
use crate::utils::session_archive::find_session_by_identifier;

use super::{
    BackgroundOutput, BackgroundOutputError, BackgroundOutputReader, BackgroundOutputSource, BackgroundTaskRef,
};

/// Default [`BackgroundOutputReader`] backed by the workspace exec sessions
/// and the session archive.
pub struct ExecSessionOutputReader {
    exec_sessions: ExecSessionManager,
}

impl ExecSessionOutputReader {
    /// Creates a reader over the controller's exec-session manager.
    #[must_use]
    pub fn new(exec_sessions: ExecSessionManager) -> Self {
        Self { exec_sessions }
    }

    /// Reads the output recorded for one resolved task source.
    async fn read_source(&self, source: BackgroundOutputSource<'_>) -> Result<BackgroundOutput, BackgroundOutputError> {
        if source.exec_session_id.is_empty() {
            return Ok(BackgroundOutput::NotReady);
        }
        match self.exec_sessions.read_session_output(source.exec_session_id, false).await {
            Ok(Some(output)) => Ok(BackgroundOutput::Live(output)),
            Ok(None) => read_archived_output(source, self.exec_sessions.workspace_root()).await,
            Err(error) if is_missing_exec_session(&error) => {
                read_archived_output(source, self.exec_sessions.workspace_root()).await
            }
            Err(error) => Err(BackgroundOutputError::Live(error)),
        }
    }
}

impl BackgroundOutputReader for ExecSessionOutputReader {
    fn read_output<'a>(
        &'a self,
        task: &'a BackgroundTaskRef<'a>,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>> {
        // The exec-session id and the recorded paths are derived here, inside
        // the adapter, from the reference the controller built.
        Box::pin(self.read_source(task.source))
    }
}

/// Reads the transcript or archive for a task whose live output is unavailable.
///
/// A record only carries a recorded path once a refresh has resolved its
/// archive, and a client may read the output of a task that has already
/// finished before any refresh has run. The task's own session identifier names
/// that archive, so it is resolved here rather than reported as no output: an
/// empty tail would claim the finished task never wrote anything.
async fn read_archived_output(
    source: BackgroundOutputSource<'_>,
    workspace_root: &Path,
) -> Result<BackgroundOutput, BackgroundOutputError> {
    let recorded = source.transcript_path.or(source.archive_path).map(Path::to_path_buf);
    let trusted = find_session_by_identifier(source.session_id)
        .await
        .map_err(BackgroundOutputError::Archived)?;
    let trusted_path = trusted.as_ref().map(|listing| listing.path.as_path());
    let resolved = match (recorded, trusted_path) {
        (Some(recorded), trusted_path) => Some(authorised_archive_path(&recorded, trusted_path, workspace_root).await?),
        (None, Some(trusted_path)) => Some(trusted_path.to_path_buf()),
        (None, None) => None,
    };
    let Some(path) = resolved else {
        return Ok(BackgroundOutput::NotReady);
    };
    // The store already returned this archive's transcript when it resolved the
    // task's identifier, so reuse it rather than reading and parsing the same
    // file a second time.
    if let Some(listing) = trusted.filter(|listing| listing.path == path) {
        return Ok(BackgroundOutput::Archived(listing.snapshot.transcript.join("\n")));
    }
    load_archive_transcript(&path)
        .await
        .map(BackgroundOutput::Archived)
        .map_err(BackgroundOutputError::Archived)
}

/// Returns the recorded archive path when the task is entitled to read it.
///
/// The ledger supplies the transcript and archive paths as stored data, so a
/// record can name any file this process is able to read, and the caller's
/// ownership check covers the record rather than the file it names. The task's
/// own archive is named by the session store, which builds the path inside the
/// sessions directory and refuses a symlink or a non-file, so a recorded path
/// is read only when it is that file or when it stays inside the workspace the
/// task belongs to. Anything else is refused as an archive fault rather than
/// served.
///
/// A path that does not exist is returned unchanged: the read that follows
/// reports the missing archive against the caller's target, which is the same
/// fault the caller saw before this check existed.
async fn authorised_archive_path(
    recorded: &Path,
    trusted: Option<&Path>,
    workspace_root: &Path,
) -> Result<std::path::PathBuf, BackgroundOutputError> {
    let metadata = match tokio::fs::symlink_metadata(recorded).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(recorded.to_path_buf()),
        Err(error) => {
            return Err(BackgroundOutputError::Archived(
                anyhow::Error::new(error).context(format!("inspect session archive {}", recorded.display())),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackgroundOutputError::Archived(anyhow::anyhow!(
            "refusing to read a recorded archive that is not a regular file: {}",
            recorded.display()
        )));
    }
    let canonical = dunce::canonicalize(recorded).map_err(|error| {
        BackgroundOutputError::Archived(
            anyhow::Error::new(error).context(format!("resolve session archive {}", recorded.display())),
        )
    })?;
    let trusted_canonical = trusted.and_then(|path| dunce::canonicalize(path).ok());
    if trusted_canonical.is_some_and(|trusted| trusted == canonical) || canonical.starts_with(workspace_root) {
        return Ok(canonical);
    }
    Err(BackgroundOutputError::Archived(anyhow::anyhow!(
        "recorded archive is outside the task's workspace and session store: {}",
        recorded.display()
    )))
}

/// Reports whether a live-output read failed because the exec session is gone.
///
/// That is the expected end state for a background subprocess whose output has
/// already been archived, so it is the only read failure that falls back to the
/// archive.
///
/// Two typed not-found causes count, from either storage layer. The
/// exec-session record is removed before its PTY handle is released, so a read
/// that races the close finds the record and misses the handle; the task has
/// simply ended, and reading its archive is the same fallback as a read that
/// misses the record. This matches the background liveness classifier, so one
/// close cannot be an exit for the status projection and a fault for the
/// output read.
fn is_missing_exec_session(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<ExecSessionNotFound>() || cause.is::<crate::tools::pty::PtySessionNotFound>())
}

#[cfg(all(test, unix))]
mod tests;
