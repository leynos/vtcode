//! Live-output and not-ready contracts for the exec-session output reader.
//!
//! Declared by `exec_session/tests/mod.rs`. These cases pin what a read returns
//! while the session is live, when it is not yet ready, and when the record or
//! its PTY handle has gone; they use the fixtures in
//! `exec_session/tests/fixtures.rs`.
use super::super::super::{BackgroundOutput, BackgroundOutputSource};
use super::fixtures::{
    SessionDirOverride, reader_environment, register_launch, wait_for_live_output, write_archive, write_stored_archive,
};
use crate::utils::path::canonicalize_workspace;
use crate::utils::session_archive::override_sessions_dir_for_tests;
use hashbrown::HashMap;
use tempfile::tempdir;

/// Live output is returned as the `Live` variant unchanged.
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
    register_launch(&manager, "live-output-child");

    let output = wait_for_live_output(&reader, "live-output").await?;

    assert_eq!(output, "live text");
    manager.close_session("live-output").await?;
    Ok(())
}

/// A not-ready live session falls back to the archive.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn not_ready_live_output_falls_back_to_the_archive() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
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
    register_launch(&manager, "quiet-output-child");
    let archive_path = write_stored_archive(&sessions_dir, "quiet-output-child")?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "quiet-output",
            session_id: "quiet-output-child",
            transcript_path: Some(&archive_path),
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::Archived("first line\nsecond line".to_string()));
    manager.close_session("quiet-output").await?;
    Ok(())
}

/// A missing exec session falls back to the archive.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn missing_exec_session_falls_back_to_the_archive() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (manager, _pty_sessions, reader) = reader_environment(&root);
    register_launch(&manager, "archived-child");
    let archive_path = write_stored_archive(&sessions_dir, "archived-child")?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "archived-child",
            transcript_path: None,
            archive_path: Some(&archive_path),
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::Archived("first line\nsecond line".to_string()));
    Ok(())
}

/// A task with no recorded path has nothing to serve.
#[tokio::test]
async fn a_task_without_a_recorded_path_stays_not_ready() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let root = canonicalize_workspace(temp.path());
    let (manager, _pty_sessions, reader) = reader_environment(&root);
    register_launch(&manager, "no-such-child-session");

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "no-such-child-session",
            transcript_path: None,
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::NotReady);
    Ok(())
}

/// An empty exec session is not ready and never reads the archive.
#[tokio::test]
async fn an_empty_exec_session_is_not_ready_without_touching_the_archive() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let archive_path = write_archive(&root)?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "",
            session_id: "empty-exec-child",
            transcript_path: Some(&archive_path),
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::NotReady);
    Ok(())
}
