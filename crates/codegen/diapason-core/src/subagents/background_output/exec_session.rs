//! Default [`ExecSessionOutputReader`] adapter over live exec sessions and the
//! session archive.
//!
//! A record's two identity fields must agree before its archive is resolved:
//! the task's `exec_session_id` must embed its `session_id` under the
//! controller's launch convention (`exec-<session>-<pid>-<sequence>`). A launch
//! writes that pair, so a row restored from the ledger — including one a
//! resumed session reads after a process restart — keeps its own archive
//! readable, while a row naming another task's session with an unrelated exec
//! id is refused as an archive fault. The recorded-path check remains as an
//! independent safeguard for the file the store resolves.
use anyhow::Result;
use futures::future::BoxFuture;
use std::path::Path;

use crate::subagents::background::load_archive_transcript;
use crate::tools::exec_session::{ExecSessionManager, controller_exec_session_task_id, is_missing_exec_session};
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
            Ok(None) => self.read_archived_output(source).await,
            Err(error) if is_missing_exec_session(&error) => self.read_archived_output(source).await,
            Err(error) => Err(BackgroundOutputError::Live(error)),
        }
    }

    /// Reads the transcript or archive for a task whose live output is
    /// unavailable.
    ///
    /// A record only carries a recorded path once a refresh has resolved its
    /// archive, and a client may read the output of a task that has already
    /// finished before any refresh has run. The task's own session identifier
    /// names that archive, so it is resolved here rather than reported as no
    /// output: an empty tail would claim the finished task never wrote anything.
    ///
    /// Both identity fields are stored data once the row is restored, so either
    /// can name another task's session. They are therefore used for the lookup
    /// only while they agree under the controller's launch convention: the exec
    /// session must embed this record's session id
    /// ([`controller_exec_session_task_id`]). A launch writes that pair, so a
    /// restored row keeps its own archive readable after a restart, while a row
    /// naming another task's session with an unrelated exec id fails closed as
    /// an archive fault.
    async fn read_archived_output(
        &self,
        source: BackgroundOutputSource<'_>,
    ) -> Result<BackgroundOutput, BackgroundOutputError> {
        if controller_exec_session_task_id(source.exec_session_id) != Some(source.session_id) {
            return Err(unverified_session(source));
        }
        let recorded = source.transcript_path.or(source.archive_path).map(Path::to_path_buf);
        // The store is the only authority for which file is this task's archive,
        // so its fault decides the read even when the record names a path.
        let trusted = find_session_by_identifier(source.session_id)
            .await
            .map_err(BackgroundOutputError::Archived)?;
        let trusted_path = trusted.as_ref().map(|listing| listing.path.as_path());
        let resolved = match recorded {
            Some(recorded) => Some(authorised_archive_path(&recorded, trusted_path).await?),
            None => trusted_path.map(Path::to_path_buf),
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

/// The refusal for a record whose two identity fields disagree.
///
/// A restored row carries both the session identifier and the exec-session
/// identifier as stored data, so resolving the session against the session
/// store could return another task's archive. A record written by a launch
/// embeds its own session id in its exec id; refusing every other pair before
/// the lookup runs keeps a row from naming any session it likes.
fn unverified_session(source: BackgroundOutputSource<'_>) -> BackgroundOutputError {
    BackgroundOutputError::Archived(anyhow::anyhow!(
        "refusing to resolve an archive for a record whose exec session id does not embed its session id: {} / {}",
        source.session_id,
        source.exec_session_id
    ))
}

/// Returns the recorded archive path only when it is this task's own archive.
///
/// The ledger supplies the transcript and archive paths as stored data, so a
/// record can name any file this process is able to read, and the caller's
/// ownership check covers the record rather than the file it names. The task's
/// own archive is the one the session store resolves for its identifier, and
/// that resolution builds the path inside the sessions directory and refuses a
/// symlink or a non-file. A recorded path is therefore served only when it
/// resolves to that same file. A path that merely exists inside the workspace
/// — another task's archive, for example — is not this task's archive and is
/// refused as an archive fault rather than served.
///
/// A path that cannot be inspected is refused too, including one that does not
/// exist: the read that follows opens the path again, so returning an absent
/// path unchecked would let a file created — or a symlink substituted — at that
/// path between this check and that open escape the rule this check exists to
/// enforce.
async fn authorised_archive_path(
    recorded: &Path,
    trusted: Option<&Path>,
) -> Result<std::path::PathBuf, BackgroundOutputError> {
    let metadata = tokio::fs::symlink_metadata(recorded).await.map_err(|error| {
        BackgroundOutputError::Archived(
            anyhow::Error::new(error).context(format!("inspect session archive {}", recorded.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackgroundOutputError::Archived(anyhow::anyhow!(
            "refusing to read a recorded archive that is not a regular file: {}",
            recorded.display()
        )));
    }
    let canonical = canonicalize_archive_path(recorded).await?;
    let Some(trusted) = trusted else {
        return Err(outside_task_scope(recorded));
    };
    let trusted_canonical = canonicalize_archive_path(trusted).await?;
    if canonical == trusted_canonical {
        return Ok(canonical);
    }
    Err(outside_task_scope(recorded))
}

/// Resolves an archive path off the async worker.
///
/// `dunce::canonicalize` performs synchronous filesystem work, so it runs on
/// the blocking pool rather than stalling an executor worker. A blocking task
/// that cannot be joined or a resolution failure is reported as the same
/// archive fault, with the path that could not be resolved.
async fn canonicalize_archive_path(path: &Path) -> Result<std::path::PathBuf, BackgroundOutputError> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || dunce::canonicalize(&owned))
        .await
        .map_err(|error| resolve_fault(path, anyhow::Error::new(error)))?
        .map_err(|error| resolve_fault(path, anyhow::Error::new(error)))
}

/// The archive fault for a path that could not be resolved.
fn resolve_fault(path: &Path, error: anyhow::Error) -> BackgroundOutputError {
    BackgroundOutputError::Archived(error.context(format!("resolve session archive {}", path.display())))
}

/// The refusal for a recorded path that is not this task's resolved archive.
fn outside_task_scope(recorded: &Path) -> BackgroundOutputError {
    BackgroundOutputError::Archived(anyhow::anyhow!(
        "recorded archive is outside the task's workspace and session store: {}",
        recorded.display()
    ))
}

#[cfg(all(test, unix))]
mod tests;
