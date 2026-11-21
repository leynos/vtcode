use super::super::super::{BackgroundOutput, BackgroundOutputSource};
use super::super::ExecSessionOutputReader;
use crate::config::PtyConfig;
use crate::tools::exec_session::ExecSessionManager;
#[cfg(feature = "tui")]
use crate::tools::pty::PtySize;
use crate::tools::registry::PtySessionManager;
use crate::utils::session_archive::clear_sessions_dir_override_for_tests;
#[cfg(feature = "tui")]
use hashbrown::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::time::{sleep, timeout};

/// Builds a manager plus reader over a fresh temp workspace.
pub(super) fn reader_environment(root: &Path) -> (ExecSessionManager, PtySessionManager, ExecSessionOutputReader) {
    let pty_sessions = PtySessionManager::new(root.to_path_buf(), PtyConfig::default());
    let manager = ExecSessionManager::new(root.to_path_buf(), pty_sessions.clone());
    let reader = ExecSessionOutputReader::new(manager.clone());
    (manager, pty_sessions, reader)
}

/// Writes a session archive with the given transcript lines at `path`.
pub(super) fn write_archive_lines(path: &Path, transcript: &[&str]) -> anyhow::Result<std::path::PathBuf> {
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
        "total_messages": transcript.len(),
        "distinct_tools": [],
        "transcript": transcript
    });
    std::fs::write(path, serde_json::to_string(&snapshot)?)?;
    Ok(path.to_path_buf())
}

/// Writes the two-line fixture archive as `root/session.json`.
pub(super) fn write_archive(root: &Path) -> anyhow::Result<std::path::PathBuf> {
    write_archive_lines(&root.join("session.json"), &["first line", "second line"])
}

/// Writes the fixture archive where the session store resolves `session_id`.
///
/// The reader only serves a recorded path that is the archive the store
/// resolves for the task's own session, so a fixture the production resolution
/// would not return cannot exercise a served read.
pub(super) fn write_stored_archive(sessions_dir: &Path, session_id: &str) -> anyhow::Result<std::path::PathBuf> {
    write_archive_lines(&sessions_dir.join(format!("{session_id}.json")), &["first line", "second line"])
}

/// Polls the reader until the spawned pipe session produces output.
pub(super) async fn wait_for_live_output(
    reader: &ExecSessionOutputReader,
    exec_session_id: &str,
) -> anyhow::Result<String> {
    timeout(Duration::from_secs(2), async {
        loop {
            let source = BackgroundOutputSource {
                exec_session_id,
                session_id: "live-output-child",
                transcript_path: None,
                archive_path: None,
            };
            match reader.read_source(source).await {
                Ok(BackgroundOutput::Live(output)) => return Ok::<String, anyhow::Error>(output),
                Ok(BackgroundOutput::NotReady | BackgroundOutput::Archived(_)) => {}
                Err(error) => return Err(anyhow::Error::new(error)),
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

/// Clears the session-directory override when a test ends, including on panic.
pub(super) struct SessionDirOverride;

impl Drop for SessionDirOverride {
    fn drop(&mut self) {
        clear_sessions_dir_override_for_tests();
    }
}

/// Starts a PTY-backed exec session and closes its handle behind the record.
#[cfg(feature = "tui")]
pub(super) async fn close_pty_handle(
    manager: &ExecSessionManager,
    pty_sessions: &PtySessionManager,
    root: &Path,
) -> anyhow::Result<()> {
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
            root.to_path_buf(),
            size,
            HashMap::new(),
            None,
        )
        .await?;
    pty_sessions.manager().close_session("pty-output")?;
    Ok(())
}
