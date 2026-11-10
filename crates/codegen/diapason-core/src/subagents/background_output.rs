//! Injectable access to live and archived background subagent output.
//!
//! The reader owns storage access only. Callers own line bounds, transport
//! limits, validation, and error mapping, so adapters can substitute their own
//! storage implementation without moving controller policy into it.
use anyhow::Result;
use futures::future::BoxFuture;
use std::path::Path;

use crate::tools::exec_session::ExecSessionManager;

use super::background::load_archive_transcript;

/// Reads background subagent output from its backing storage.
///
/// The reader owns the storage access only. Callers own line bounds, transport
/// limits, validation and error mapping. `Ok(None)` from
/// [`Self::read_live_output`] means "live output not ready yet", and only a
/// missing exec session (`ExecSessionNotFound`) may fall back to the archive.
pub trait BackgroundOutputReader: Send + Sync {
    /// Returns the live output recorded for an exec session.
    ///
    /// The reader owns the storage access only. `Ok(None)` means live output
    /// is not ready yet; callers decide whether and when to read the archive.
    /// Only a missing exec session may be mapped to archive fallback.
    fn read_live_output<'a>(&'a self, exec_session_id: &'a str) -> BoxFuture<'a, Result<Option<String>>>;

    /// Returns the full archived transcript stored at `path`.
    ///
    /// The reader owns the storage access only; bounding the transcript is the
    /// caller's responsibility.
    fn read_archived_output<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<String>>;
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
    fn read_live_output<'a>(&'a self, exec_session_id: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { self.exec_sessions.read_session_output(exec_session_id, false).await })
    }

    fn read_archived_output<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<String>> {
        Box::pin(load_archive_transcript(path))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{BackgroundOutputReader, ExecSessionOutputReader};
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

    /// Polls the reader until the spawned pipe session produces output.
    async fn wait_for_live_output(reader: &ExecSessionOutputReader, session_id: &str) -> anyhow::Result<String> {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Some(output) = reader.read_live_output(session_id).await? {
                    return Ok::<String, anyhow::Error>(output);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await?
    }

    #[tokio::test]
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
    async fn not_ready_live_output_stays_none() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (manager, _pty_sessions, reader) = reader_environment(&root);
        manager
            .create_pipe_session(
                "quiet-output".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                root,
                HashMap::new(),
            )
            .await?;

        let output = reader.read_live_output("quiet-output").await?;

        assert_eq!(output, None);
        manager.close_session("quiet-output").await?;
        Ok(())
    }

    #[tokio::test]
    async fn missing_exec_session_propagates_the_typed_error() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (_manager, _pty_sessions, reader) = reader_environment(&root);

        let error = reader
            .read_live_output("absent-session")
            .await
            .expect_err("an unknown exec session must fail");

        assert!(
            error.chain().any(|cause| cause.is::<ExecSessionNotFound>()),
            "the controller needs the typed missing-session cause to choose archive fallback"
        );
        Ok(())
    }

    /// A PTY handle removed out from under the exec-session record is not a
    /// missing exec session, so the reader must propagate it unchanged.
    #[tokio::test]
    #[cfg(feature = "tui")]
    async fn unrelated_read_error_is_propagated_unchanged() -> anyhow::Result<()> {
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
            .read_live_output("pty-output")
            .await
            .expect_err("a removed PTY handle must surface as a read error");

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

    #[tokio::test]
    async fn archived_transcript_is_returned_without_a_line_bound() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let root = canonicalize_workspace(temp.path());
        let (_manager, _pty_sessions, reader) = reader_environment(&root);
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

        let transcript = reader.read_archived_output(&archive_path).await?;

        assert_eq!(transcript, "first line\nsecond line");
        Ok(())
    }
}
