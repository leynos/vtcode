//! Injectable access to live and archived background subagent output.
//!
//! A reader is keyed by one background task's storage identity and returns a
//! domain result, so a controller never classifies exec-session errors or
//! chooses between the transcript and archive paths. Callers still own line
//! bounds, transport limits, validation and error mapping.
use anyhow::Result;
use futures::future::BoxFuture;
use std::path::Path;

use crate::tools::exec_session::{ExecSessionManager, ExecSessionNotFound};

use super::background::load_archive_transcript;
use super::types::BackgroundSubprocessEntry;

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

/// The storage identities of one background task's output.
///
/// The controller derives this from the task id it already resolved. A reader
/// therefore never needs ledger access, and it decides which recorded path to
/// read when live output is unavailable.
#[derive(Debug, Clone, Copy)]
pub struct BackgroundOutputSource<'a> {
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
    pub fn from_entry(entry: &'a BackgroundSubprocessEntry) -> Self {
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
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live(error) => write!(formatter, "read live background output: {error}"),
            Self::Archived(error) => write!(formatter, "read archived background output: {error}"),
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
/// archive-path selection. Callers own line bounds, transport limits,
/// validation and error mapping.
pub trait BackgroundOutputReader: Send + Sync {
    /// Reads the output the task can serve.
    ///
    /// Live output wins. A not-ready live session and a missing exec session
    /// fall back to the transcript or archive. Any other live-read fault is
    /// returned as [`BackgroundOutputError::Live`], so it is never disguised as
    /// a missing session or replaced by archived output.
    fn read_output<'a>(
        &'a self,
        source: BackgroundOutputSource<'a>,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>>;
}

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
}

impl BackgroundOutputReader for ExecSessionOutputReader {
    fn read_output<'a>(
        &'a self,
        source: BackgroundOutputSource<'a>,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>> {
        Box::pin(async move {
            if source.exec_session_id.is_empty() {
                return Ok(BackgroundOutput::NotReady);
            }
            match self.exec_sessions.read_session_output(source.exec_session_id, false).await {
                Ok(Some(output)) => Ok(BackgroundOutput::Live(output)),
                Ok(None) => read_archived_output(source).await,
                Err(error) if is_missing_exec_session(&error) => read_archived_output(source).await,
                Err(error) => Err(BackgroundOutputError::Live(error)),
            }
        })
    }
}

/// Reads the transcript or archive for a task whose live output is unavailable.
async fn read_archived_output(source: BackgroundOutputSource<'_>) -> Result<BackgroundOutput, BackgroundOutputError> {
    let Some(path) = source.transcript_path.or(source.archive_path) else {
        return Ok(BackgroundOutput::NotReady);
    };
    load_archive_transcript(path)
        .await
        .map(BackgroundOutput::Archived)
        .map_err(BackgroundOutputError::Archived)
}

/// Reports whether a live-output read failed because the exec session is gone.
///
/// That is the expected end state for a background subprocess whose output has
/// already been archived, so it is the only read failure that falls back to the
/// archive.
fn is_missing_exec_session(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<ExecSessionNotFound>())
}

#[cfg(all(test, unix))]
mod tests {
    //! Exercises the default [`ExecSessionOutputReader`] adapter.
    //!
    //! Each case pins one contract the controller relies on when it reads
    //! through [`BackgroundOutputReader`]: live output is returned unchanged, a
    //! not-ready session falls back to the archive, a missing exec session
    //! falls back to the archive, an unrelated read fault is not disguised as
    //! a missing session, a task without a recorded path stays `NotReady`, and
    //! an unreadable archive is reported as an archive fault.
    use super::{
        BackgroundOutput, BackgroundOutputError, BackgroundOutputReader, BackgroundOutputSource,
        ExecSessionOutputReader,
    };
    use crate::config::PtyConfig;
    use crate::tools::exec_session::{ExecSessionManager, ExecSessionNotFound};
    #[cfg(feature = "tui")]
    use crate::tools::pty::PtySize;
    use crate::tools::registry::PtySessionManager;
    use crate::utils::path::canonicalize_workspace;
    use hashbrown::HashMap;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time::{sleep, timeout};

    /// Builds a manager plus reader over a fresh temp workspace.
    fn reader_environment(root: &Path) -> (ExecSessionManager, PtySessionManager, ExecSessionOutputReader) {
        let pty_sessions = PtySessionManager::new(root.to_path_buf(), PtyConfig::default());
        let manager = ExecSessionManager::new(root.to_path_buf(), pty_sessions.clone());
        let reader = ExecSessionOutputReader::new(manager.clone());
        (manager, pty_sessions, reader)
    }

    /// Writes a two-line session archive and returns its path.
    fn write_archive(root: &Path) -> anyhow::Result<std::path::PathBuf> {
        let archive_path = root.join("session.json");
        let snapshot = serde_json::json!({
            "metadata": {
                "workspace_label": "workspace",
                "workspace_path": "/workspace",
                "model": "model",
                "provider": "provider",
                "theme": "dark",
                "reasoning_effort": "medium"
            },
            "started_at": "2026-08-28T00:00:00Z",
            "ended_at": "2026-08-28T00:00:01Z",
            "total_messages": 2,
            "distinct_tools": [],
            "transcript": ["first line", "second line"]
        });
        std::fs::write(&archive_path, serde_json::to_string(&snapshot)?)?;
        Ok(archive_path)
    }

    /// Polls the reader until the spawned pipe session produces output.
    async fn wait_for_live_output(reader: &ExecSessionOutputReader, exec_session_id: &str) -> anyhow::Result<String> {
        timeout(Duration::from_secs(2), async {
            loop {
                let source = BackgroundOutputSource {
                    exec_session_id,
                    transcript_path: None,
                    archive_path: None,
                };
                match reader.read_output(source).await {
                    Ok(BackgroundOutput::Live(output)) => return Ok::<String, anyhow::Error>(output),
                    Ok(BackgroundOutput::NotReady | BackgroundOutput::Archived(_)) => {}
                    Err(error) => return Err(anyhow::Error::new(error)),
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await?
    }

    #[tokio::test]
    /// Live output is returned as the `Live` variant unchanged.
    async fn live_output_is_returned_unchanged() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (manager, _pty_sessions, reader) = reader_environment(&root);
        manager
            .create_pipe_session(
                "live-output".to_string().into(),
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "printf 'live text'".to_string(),
                ],
                root,
                HashMap::new(),
            )
            .await?;

        let output = wait_for_live_output(&reader, "live-output").await?;

        assert_eq!(output, "live text");
        manager.close_session("live-output").await?;
        Ok(())
    }

    #[tokio::test]
    /// A not-ready live session falls back to the archive.
    async fn not_ready_live_output_falls_back_to_the_archive() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (manager, _pty_sessions, reader) = reader_environment(&root);
        manager
            .create_pipe_session(
                "quiet-output".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                root.clone(),
                HashMap::new(),
            )
            .await?;
        let archive_path = write_archive(&root)?;

        let output = reader
            .read_output(BackgroundOutputSource {
                exec_session_id: "quiet-output",
                transcript_path: Some(&archive_path),
                archive_path: None,
            })
            .await
            .map_err(anyhow::Error::new)?;

        assert_eq!(output, BackgroundOutput::Archived("first line\nsecond line".to_string()));
        manager.close_session("quiet-output").await?;
        Ok(())
    }

    #[tokio::test]
    /// A missing exec session falls back to the archive.
    async fn missing_exec_session_falls_back_to_the_archive() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (_manager, _pty_sessions, reader) = reader_environment(&root);
        let archive_path = write_archive(&root)?;

        let output = reader
            .read_output(BackgroundOutputSource {
                exec_session_id: "absent-session",
                transcript_path: None,
                archive_path: Some(&archive_path),
            })
            .await
            .map_err(anyhow::Error::new)?;

        assert_eq!(output, BackgroundOutput::Archived("first line\nsecond line".to_string()));
        Ok(())
    }

    #[tokio::test]
    /// A task with no recorded path has nothing to serve.
    async fn a_task_without_a_recorded_path_stays_not_ready() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (_manager, _pty_sessions, reader) = reader_environment(&root);

        let output = reader
            .read_output(BackgroundOutputSource {
                exec_session_id: "absent-session",
                transcript_path: None,
                archive_path: None,
            })
            .await
            .map_err(anyhow::Error::new)?;

        assert_eq!(output, BackgroundOutput::NotReady);
        Ok(())
    }

    #[tokio::test]
    /// An empty exec session is not ready and never reads the archive.
    async fn an_empty_exec_session_is_not_ready_without_touching_the_archive() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (_manager, _pty_sessions, reader) = reader_environment(&root);
        let archive_path = write_archive(&root)?;

        let output = reader
            .read_output(BackgroundOutputSource {
                exec_session_id: "",
                transcript_path: Some(&archive_path),
                archive_path: None,
            })
            .await
            .map_err(anyhow::Error::new)?;

        assert_eq!(output, BackgroundOutput::NotReady);
        Ok(())
    }

    #[tokio::test]
    /// An unreadable archive is reported as an archive fault.
    async fn an_unreadable_archive_is_an_archive_fault() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (_manager, _pty_sessions, reader) = reader_environment(&root);
        let archive_path = root.join("missing-session.json");

        let error = reader
            .read_output(BackgroundOutputSource {
                exec_session_id: "absent-session",
                transcript_path: Some(&archive_path),
                archive_path: None,
            })
            .await
            .expect_err("an unreadable archive must fail");

        assert!(
            matches!(error, BackgroundOutputError::Archived(_)),
            "a failed archive read must be reported as an archive fault: {error}"
        );
        Ok(())
    }

    /// A PTY handle removed out from under the exec-session record is not a
    /// missing exec session, so the reader must propagate it as a live fault.
    #[tokio::test]
    #[cfg(feature = "tui")]
    async fn unrelated_read_error_is_a_live_fault() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (manager, pty_sessions, reader) = reader_environment(&root);
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        manager
            .create_pty_session(
                "pty-output".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                root,
                size,
                HashMap::new(),
                None,
            )
            .await?;
        pty_sessions.manager().close_session("pty-output")?;

        let error = reader
            .read_output(BackgroundOutputSource {
                exec_session_id: "pty-output",
                transcript_path: None,
                archive_path: None,
            })
            .await
            .expect_err("a removed PTY handle must surface as a read error");

        let BackgroundOutputError::Live(error) = &error else {
            panic!("an environmental read fault must stay a live fault: {error}");
        };
        assert!(
            !error.chain().any(|cause| cause.is::<ExecSessionNotFound>()),
            "an environmental read fault must stay distinct from a missing exec session"
        );
        assert!(
            error.to_string().contains("PTY session 'pty-output' not found"),
            "the reader must propagate the original fault: {error}"
        );
        Ok(())
    }
}
