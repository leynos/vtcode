//! Injectable access to live and archived background subagent output.
//!
//! A reader is keyed by one background task's identity. The controller builds a
//! [`BackgroundTaskRef`] from the live [`BackgroundSubprocessEntry`] it already
//! holds, and the exec-session adapter derives the storage handles from the
//! reference's source. The controller passes its live record rather than the
//! persisted ledger copy, because the ledger copy can lag the running task; the
//! adapter must therefore not re-read the ledger on every poll. Callers still
//! own line bounds, transport limits, validation and error mapping.
use anyhow::Result;
use futures::future::BoxFuture;
use std::path::Path;

use super::types::BackgroundSubprocessEntry;

mod exec_session;

pub use exec_session::ExecSessionOutputReader;

/// The output one background task can serve for a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundOutput {
    /// Output recorded by the task's live exec session.
    Live(String),
    /// Output recovered from the task's transcript or session archive.
    Archived(String),
    /// The task has no output to serve yet.
    NotReady,
}

/// The storage handles for one background task's output.
///
/// This stays inside the crate: callers key reads by task identity through
/// [`BackgroundOutputReader`], and only the exec-session adapter turns a task
/// reference into exec-session and path handles. A reader therefore never needs
/// ledger access, and it decides which recorded path to read when live output
/// is unavailable.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BackgroundOutputSource<'a> {
    /// The exec session that records live output; empty when none was created.
    pub(crate) exec_session_id: &'a str,
    /// The child session the task recorded, which names its archive.
    ///
    /// A record whose exec session is gone has an archive of its own even when
    /// no path was recorded yet, because the archive metadata is only attached
    /// once a refresh has resolved it. The reader uses this identifier to find
    /// that archive when it has no recorded path to read.
    pub(crate) session_id: &'a str,
    /// The task's own transcript, preferred over the session archive.
    pub(crate) transcript_path: Option<&'a Path>,
    /// The session archive, used when no transcript is recorded.
    pub(crate) archive_path: Option<&'a Path>,
}

impl<'a> BackgroundOutputSource<'a> {
    /// Builds the source for one background task projection.
    ///
    /// The transcript is preferred over the archive when both are recorded,
    /// because the transcript is the task's own record.
    #[must_use]
    fn from_entry(entry: &'a BackgroundSubprocessEntry) -> Self {
        Self {
            exec_session_id: &entry.exec_session_id,
            session_id: &entry.session_id,
            transcript_path: entry.transcript_path.as_deref(),
            archive_path: entry.archive_path.as_deref(),
        }
    }
}

/// The background task a reader is asked to read.
///
/// A reader resolves one task's output from its identity and from the storage
/// handles its live record names. Passing this reference instead of the whole
/// [`BackgroundSubprocessEntry`] keeps a reader from depending on the record's
/// status, ownership, labels or timings, which it has no use for.
///
/// Only [`id`](Self::id) is part of the public surface. The exec-session
/// identifier and the recorded transcript and archive paths stay inside the
/// exec-session adapter, which is the only reader that has storage to resolve
/// them against; an adapter outside this crate serves output from the task
/// identity alone.
///
/// The rendering is written by hand rather than derived for the same reason: a
/// derived one is reachable from every caller and would print the handles that
/// the port exists to keep inside the adapter.
#[derive(Clone, Copy)]
pub struct BackgroundTaskRef<'a> {
    /// The record id of the task, which keys the read for every reader.
    pub id: &'a str,
    /// Where the task's output may be found, resolved from its live record.
    ///
    /// Deliberately not public: the handles belong to the adapter that reads
    /// them, not to the trait's contract.
    pub(crate) source: BackgroundOutputSource<'a>,
}

impl std::fmt::Debug for BackgroundTaskRef<'_> {
    /// Renders the task identity and nothing else.
    ///
    /// Formatting is reachable from every caller, so the storage handles are
    /// omitted: a reader outside this crate must not be able to print an
    /// exec-session identifier or a recorded path it was never granted.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("BackgroundTaskRef").field("id", &self.id).finish()
    }
}

impl<'a> BackgroundTaskRef<'a> {
    /// Builds the reference for one background task projection.
    ///
    /// The record is the controller's live copy, so the reference names the
    /// exec session and paths the running task actually uses rather than the
    /// possibly stale persisted copy.
    #[must_use]
    pub(crate) fn from_entry(entry: &'a BackgroundSubprocessEntry) -> Self {
        Self {
            id: &entry.id,
            source: BackgroundOutputSource::from_entry(entry),
        }
    }
}

/// Why a background output read failed.
///
/// The distinction lets a best-effort caller, such as a snapshot preview,
/// ignore an unreadable archive while still surfacing a live-read fault.
#[derive(Debug)]
pub enum BackgroundOutputError {
    /// The live exec session could not be read.
    Live(anyhow::Error),
    /// The task's transcript or archive could not be read.
    Archived(anyhow::Error),
}

impl std::fmt::Display for BackgroundOutputError {
    /// Renders only this error's own context.
    ///
    /// The underlying cause is exposed through [`std::error::Error::source`],
    /// so a chain-aware renderer such as `format!("{error:#}")` prints it
    /// exactly once instead of repeating it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live(_) => formatter.write_str("read live background output"),
            Self::Archived(_) => formatter.write_str("read archived background output"),
        }
    }
}

impl std::error::Error for BackgroundOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Live(error) | Self::Archived(error) => Some(error.as_ref()),
        }
    }
}

/// Reads background subagent output from its backing storage.
///
/// The reader owns storage access, exec-session error classification and
/// archive-path selection, keyed by the task identity of the record it is
/// given. Callers own line bounds, transport limits, validation and error
/// mapping.
pub trait BackgroundOutputReader: Send + Sync {
    /// Reads the output the task can serve.
    ///
    /// The task identity is [`BackgroundTaskRef::id`]: the caller passes the
    /// record it already holds, so the adapter never re-reads the ledger to
    /// resolve the task.
    ///
    /// Live output wins. A not-ready live session and a missing exec session
    /// fall back to the transcript or archive. Any other live-read fault is
    /// returned as [`BackgroundOutputError::Live`], so it is never disguised as
    /// a missing session or replaced by archived output.
    fn read_output<'a>(
        &'a self,
        task: &'a BackgroundTaskRef<'a>,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A task reference must render its identity without its storage handles.
    ///
    /// A reader outside this crate can format the reference it is given, so a
    /// rendering that included the source would publish the exec-session
    /// identifier and the recorded paths the port exists to keep inside the
    /// adapter.
    #[test]
    fn debug_rendering_names_only_the_task_id() {
        let source = BackgroundOutputSource {
            exec_session_id: "exec-session-that-must-not-leak",
            session_id: "child-session-that-must-not-leak",
            transcript_path: Some(Path::new("/tmp/transcript-that-must-not-leak.jsonl")),
            archive_path: Some(Path::new("/tmp/archive-that-must-not-leak.jsonl")),
        };
        let task = BackgroundTaskRef { id: "task-7", source };

        let rendered = format!("{task:?}");

        assert!(rendered.contains("task-7"), "the rendering must name the task identity: {rendered}");
        for handle in [
            source.exec_session_id,
            source.session_id,
            "/tmp/transcript-that-must-not-leak.jsonl",
            "/tmp/archive-that-must-not-leak.jsonl",
        ] {
            assert!(!rendered.contains(handle), "the rendering must not publish `{handle}`: {rendered}");
        }
    }
}
