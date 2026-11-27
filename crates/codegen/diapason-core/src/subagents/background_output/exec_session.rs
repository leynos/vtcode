//! Default [`ExecSessionOutputReader`] adapter over live exec sessions and the
//! session archive.
//!
//! A record's two identity fields must agree before its output is read at all:
//! the task's `exec_session_id` must embed its `session_id` under the
//! controller's launch convention (`exec-<session>-<pid>-<sequence>`). A launch
//! writes that pair, so a row restored from the ledger — including one a
//! resumed session reads after a process restart — keeps its own archive
//! readable, while a row naming another task's session with an unrelated exec
//! id is refused before its live session or archive is touched. A recorded path
//! is authorized only against the file the store resolves, and the transcript
//! served is that store's own snapshot: the archive is read once, so no second
//! open can serve a file swapped in after the check.
use anyhow::Result;
use futures::future::BoxFuture;
use std::path::Path;

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
    ///
    /// The two identity fields are checked against each other before either
    /// storage layer is read. They are stored data once a row is restored, so
    /// a forged pair could otherwise reach another task's live exec session and
    /// be served as this task's output; the archive lookup was the only path
    /// that refused it, and a live read returned first.
    async fn read_source(&self, source: BackgroundOutputSource<'_>) -> Result<BackgroundOutput, BackgroundOutputError> {
        if source.exec_session_id.is_empty() {
            return Ok(BackgroundOutput::NotReady);
        }
        if controller_exec_session_task_id(source.exec_session_id) != Some(source.session_id) {
            return Err(unverified_session(source));
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
    /// The caller has already verified that the record's two identity fields
    /// agree under the controller's launch convention: the exec session embeds
    /// this record's session id
    /// ([`controller_exec_session_task_id`]). A launch writes that pair, so a
    /// restored row keeps its own archive readable after a restart, while a row
    /// naming another task's session with an unrelated exec id fails closed
    /// before either storage layer is read.
    async fn read_archived_output(
        &self,
        source: BackgroundOutputSource<'_>,
    ) -> Result<BackgroundOutput, BackgroundOutputError> {
        let recorded = source.transcript_path.or(source.archive_path).map(Path::to_path_buf);
        // The store is the only authority for which file is this task's archive,
        // so its fault decides the read even when the record names a path.
        let trusted = find_session_by_identifier(source.session_id)
            .await
            .map_err(BackgroundOutputError::Archived)?;
        let trusted_path = trusted.as_ref().map(|listing| listing.path.as_path());
        if let Some(recorded) = recorded.as_deref() {
            // A recorded path is only ever accepted against the file the store
            // resolved, so the check runs before the store's answer is used.
            authorize_archive_path(recorded, trusted_path).await?;
        }
        let Some(listing) = trusted else {
            // Nothing is recorded for this task, and any recorded path was
            // refused above: there is no archive to serve rather than a fault.
            return Ok(BackgroundOutput::NotReady);
        };
        // The store's own snapshot is served, so the archive is never opened a
        // second time. Reopening the authorized path would let a file swapped in
        // after the check be read as this task's output.
        Ok(BackgroundOutput::Archived(listing.snapshot.transcript.join("\n")))
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

/// Accepts a recorded archive path only when it is the file the store resolved.
///
/// The ledger supplies the transcript and archive paths as stored data, so a
/// record can name any file this process is able to read, and the caller's
/// ownership check covers the record rather than the file it names. The task's
/// own archive is the one the session store resolves for its identifier, and
/// that resolution builds the path inside the sessions directory and refuses a
/// symlink or a non-file. A recorded path is therefore accepted only when it
/// resolves to that same file. A path that merely exists inside the workspace
/// — another task's archive, for example — is not this task's archive and is
/// refused as an archive fault rather than served.
///
/// The check returns no path. The caller serves the store's own snapshot, so no
/// second handle is opened: a path swapped between this check and a reopen could
/// otherwise serve a file the store never authorized, and the check exists to
/// prevent exactly that.
///
/// A path that cannot be inspected is refused too, including one that does not
/// exist, for the same reason: an absent path now may be a symlink by the time
/// anything opened it.
async fn authorize_archive_path(recorded: &Path, trusted: Option<&Path>) -> Result<(), BackgroundOutputError> {
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
        return Ok(());
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
mod tests {
    //! Exercises the default [`ExecSessionOutputReader`](super::ExecSessionOutputReader)
    //! adapter.
    //!
    //! Each case pins one contract the controller relies on when it reads
    //! through [`BackgroundOutputReader`](super::super::BackgroundOutputReader):
    //! live output is returned unchanged, a not-ready session falls back to the
    //! archive, a missing exec session or
    //! a removed PTY handle falls back to the archive resolved for the task's own
    //! session, a recorded path that is another task's archive is refused, a
    //! recorded path outside the task's scope is refused, a recorded path absent
    //! when it is checked is refused rather than served, a record whose exec
    //! session id does not embed its session id is refused as an archive fault, a
    //! record restored from the ledger still serves its own archive, a caller
    //! chosen `--session-id` authorizes nothing, a task without a recorded path
    //! stays `NotReady`, and an unreadable archive is reported as an archive fault.

    mod archive;
    mod fixtures;
    mod live;
}
