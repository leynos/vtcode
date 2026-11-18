//! Injectable access to live and archived background subagent output.
//!
//! A reader is keyed by one background task's identity. The controller passes
//! the live [`BackgroundSubprocessEntry`] it already holds, and the adapter
//! derives the storage handles from that record. The controller passes its live
//! record rather than the persisted ledger copy, because the ledger copy can lag
//! the running task; the adapter must therefore not re-read the ledger on every
//! poll. Callers still own line bounds, transport limits, validation and error
//! mapping.
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
/// This is private to the adapter: callers key reads by task identity through
/// [`BackgroundOutputReader`], and only the adapter turns a task record into
/// exec-session and path handles. A reader therefore never needs ledger access,
/// and it decides which recorded path to read when live output is unavailable.
#[derive(Debug, Clone, Copy)]
struct BackgroundOutputSource<'a> {
    /// The exec session that records live output; empty when none was created.
    pub exec_session_id: &'a str,
    /// The task's own transcript, preferred over the session archive.
    pub transcript_path: Option<&'a Path>,
    /// The session archive, used when no transcript is recorded.
    pub archive_path: Option<&'a Path>,
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
            transcript_path: entry.transcript_path.as_deref(),
            archive_path: entry.archive_path.as_deref(),
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
    /// The task identity is `entry.id`: the caller passes the record it already
    /// holds, so the adapter never re-reads the ledger to resolve the task.
    ///
    /// Live output wins. A not-ready live session and a missing exec session
    /// fall back to the transcript or archive. Any other live-read fault is
    /// returned as [`BackgroundOutputError::Live`], so it is never disguised as
    /// a missing session or replaced by archived output.
    fn read_output<'a>(
        &'a self,
        entry: &'a BackgroundSubprocessEntry,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>>;
}
