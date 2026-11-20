//! Exercises the default [`ExecSessionOutputReader`] adapter.
//!
//! Each case pins one contract the controller relies on when it reads
//! through [`BackgroundOutputReader`]: live output is returned unchanged, a
//! not-ready session falls back to the archive, a missing exec session or
//! a removed PTY handle falls back to the archive resolved for the task's own
//! session, a recorded path that is another task's archive is refused, a
//! recorded path outside the task's scope is refused, a recorded path absent
//! when it is checked is refused rather than served, a task without a recorded
//! path stays `NotReady`, and an unreadable archive is reported as an archive
//! fault.
use super::super::{BackgroundOutput, BackgroundOutputError, BackgroundOutputSource};
use super::ExecSessionOutputReader;
use crate::config::PtyConfig;
use crate::tools::exec_session::{ExecSessionManager, ExecSessionNotFound, is_missing_exec_session};
#[cfg(feature = "tui")]
use crate::tools::pty::PtySize;
use crate::tools::registry::PtySessionManager;
use crate::utils::path::canonicalize_workspace;
use crate::utils::session_archive::{clear_sessions_dir_override_for_tests, override_sessions_dir_for_tests};
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

/// Writes a session archive with the given transcript lines at `path`.
fn write_archive_lines(path: &Path, transcript: &[&str]) -> anyhow::Result<std::path::PathBuf> {
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
fn write_archive(root: &Path) -> anyhow::Result<std::path::PathBuf> {
    write_archive_lines(&root.join("session.json"), &["first line", "second line"])
}

/// Writes the fixture archive where the session store resolves `session_id`.
///
/// The reader only serves a recorded path that is the archive the store
/// resolves for the task's own session, so a fixture the production resolution
/// would not return cannot exercise a served read.
fn write_stored_archive(sessions_dir: &Path, session_id: &str) -> anyhow::Result<std::path::PathBuf> {
    write_archive_lines(&sessions_dir.join(format!("{session_id}.json")), &["first line", "second line"])
}

/// Polls the reader until the spawned pipe session produces output.
async fn wait_for_live_output(reader: &ExecSessionOutputReader, exec_session_id: &str) -> anyhow::Result<String> {
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
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
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
    let (_manager, _pty_sessions, reader) = reader_environment(&root);

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

/// Clears the session-directory override when a test ends, including on panic.
struct SessionDirOverride;

impl Drop for SessionDirOverride {
    fn drop(&mut self) {
        clear_sessions_dir_override_for_tests();
    }
}

/// A session-store fault decides the read even when the record names a path.
///
/// The store is the only authority for which file is this task's archive, so a
/// recorded path cannot be authorised while that lookup fails. Serving it on
/// the record's word alone would let a record read any file the workspace
/// contains, which is the cross-task read this adapter refuses.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn a_session_store_fault_refuses_a_recorded_archive() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    // An unparseable archive makes the store lookup for this identifier fail.
    std::fs::write(sessions_dir.join("broken-session.json"), "{ not an archive")?;

    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let archive_path = write_archive(&root)?;

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "finished-exec-session",
            session_id: "broken-session",
            transcript_path: None,
            archive_path: Some(&archive_path),
        })
        .await
        .expect_err("a recorded path cannot be authorised while the store lookup faults");

    assert!(
        matches!(error, BackgroundOutputError::Archived(_)),
        "a failed store lookup must be reported as an archive fault: {error}"
    );
    Ok(())
}

/// A finished task resolves its archive before a refresh records its path.
///
/// A record keeps the child session identifier it was created with, and the
/// archive is named by that identifier. A client may read the output of a
/// task that has already finished before any refresh has attached the
/// archive path to its record; reporting no output would claim the task
/// never wrote anything.
#[serial_test::serial(session_dir_override)]
#[tokio::test]
async fn a_finished_task_resolves_its_archive_without_a_recorded_path() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let archive_path = write_archive(&sessions_dir)?;
    let identifier = archive_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow::anyhow!("missing archive file stem"))?;

    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "finished-exec-session",
            session_id: identifier,
            transcript_path: None,
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::Archived("first line\nsecond line".to_string()));
    Ok(())
}

/// An unreadable archive is reported as an archive fault.
#[tokio::test]
async fn an_unreadable_archive_is_an_archive_fault() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let archive_path = root.join("missing-session.json");

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "unreadable-archive-child",
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

/// A recorded archive absent when its path is checked is refused, not served.
///
/// Returning the path unchanged would let a file created — or a symlink
/// substituted — at that path between the check and the read escape the
/// containment rule the check exists to enforce. The refusal must be an
/// archive fault so a caller sees a storage failure rather than an empty
/// preview.
#[tokio::test]
async fn a_recorded_but_absent_archive_is_an_archive_fault() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let absent = root.join("absent-archive.json");

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "absent-archive-child",
            transcript_path: Some(&absent),
            archive_path: None,
        })
        .await
        .expect_err("a recorded archive absent at check time must not be served");

    let BackgroundOutputError::Archived(error) = &error else {
        panic!("an absent recorded archive must be reported as an archive fault: {error}");
    };
    assert!(!absent.exists(), "the reader must not create the archive it refused");
    assert!(
        error.to_string().contains("inspect session archive"),
        "the fault must name the archive it refused: {error}"
    );
    Ok(())
}

/// Starts a PTY-backed exec session and closes its handle behind the record.
#[cfg(feature = "tui")]
async fn close_pty_handle(
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

/// A PTY handle removed out from under the exec-session record is an exit.
///
/// The record is removed before its handle is released, so a read that
/// races the close misses the handle while the record still exists. That
/// is the same expected end as a read that misses the record, so it falls
/// back to the archive instead of being reported as a live fault. The archive
/// is the one the session store resolves for the task's own session, which is
/// what a record carries before a refresh has attached its paths.
#[tokio::test]
#[cfg(feature = "tui")]
#[serial_test::serial(session_dir_override)]
async fn a_removed_pty_handle_falls_back_to_the_archive() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (manager, pty_sessions, reader) = reader_environment(&root);
    write_stored_archive(&sessions_dir, "pty-output-child")?;
    close_pty_handle(&manager, &pty_sessions, &root).await?;
    // Prove the fixture recreates the race the fallback exists for: the record
    // survives and its handle is gone.
    let live = manager.read_session_output("pty-output", false).await;
    assert!(
        matches!(&live, Err(error) if is_missing_exec_session(error)),
        "the fixture must remove the PTY handle behind the record: {live:?}"
    );

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "pty-output",
            session_id: "pty-output-child",
            transcript_path: None,
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(
        output,
        BackgroundOutput::Archived("first line\nsecond line".to_string()),
        "a closed PTY handle must serve the archive resolved for the task's own session"
    );
    Ok(())
}

/// The same exit with no archive anywhere still has nothing to serve.
#[tokio::test]
#[cfg(feature = "tui")]
#[serial_test::serial(session_dir_override)]
async fn a_removed_pty_handle_without_an_archive_stays_not_ready() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (manager, pty_sessions, reader) = reader_environment(&root);
    close_pty_handle(&manager, &pty_sessions, &root).await?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "pty-output",
            session_id: "pty-output-child",
            transcript_path: None,
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::NotReady, "a closed PTY handle with no archive has nothing to serve");
    Ok(())
}

/// Only a typed not-found from either storage layer is an expected exit.
///
/// This pins the shared classifier the liveness projection and the output
/// adapter both use, so the two cannot drift apart. The classification decides
/// whether a read falls back to the archive, so a plain fault must stay a fault
/// even when a caller wraps it in context.
#[test]
fn the_missing_session_classifier_accepts_both_storage_layers() {
    let record_gone = anyhow::Error::new(ExecSessionNotFound { session_id: "exec-background-worker".to_owned() })
        .context("read live output");
    assert!(is_missing_exec_session(&record_gone), "a removed record is an exit");

    let handle_gone =
        anyhow::Error::new(crate::tools::pty::PtySessionNotFound { session_id: "exec-background-worker".to_owned() });
    assert!(is_missing_exec_session(&handle_gone), "a removed PTY handle is an exit");

    let fault = anyhow::anyhow!("exec session store is unavailable");
    assert!(!is_missing_exec_session(&fault), "an untyped fault is not an exit");
}

/// A recorded path outside the workspace and the session store is refused.
///
/// The record is supplied by the workspace ledger as stored data, so a
/// record can name any file this process can read. The caller's ownership
/// check covers the record, not the file it names, so the reader must not
/// serve a file the task has no claim to.
#[tokio::test]
async fn a_recorded_archive_outside_the_task_scope_is_refused() -> anyhow::Result<()> {
    let workspace = tempdir()?;
    let elsewhere = tempdir()?;
    let root = canonicalize_workspace(workspace.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let foreign_archive = write_archive(elsewhere.path())?;

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "foreign-archive-child",
            transcript_path: Some(&foreign_archive),
            archive_path: None,
        })
        .await
        .expect_err("an archive outside the task's scope must not be served");

    let BackgroundOutputError::Archived(error) = &error else {
        panic!("a refused archive must be reported as an archive fault: {error}");
    };
    assert!(
        error.to_string().contains("outside the task's workspace"),
        "the refusal must name the containment boundary: {error}"
    );
    Ok(())
}

/// A record naming another task's archive inside the workspace is refused.
///
/// Every task's files share the workspace, so containment alone cannot decide
/// which archive a record may read: another task's valid archive also lies
/// inside it. Only the archive the session store resolves for this task's own
/// identifier is this task's to serve.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn a_record_naming_another_tasks_archive_is_refused() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    write_stored_archive(&sessions_dir, "own-child")?;
    let foreign_archive = write_archive_lines(&sessions_dir.join("other-child.json"), &["another task's line"])?;
    assert!(
        foreign_archive.starts_with(root.as_path()),
        "the fixture must place the foreign archive inside the workspace"
    );

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "own-child",
            transcript_path: Some(&foreign_archive),
            archive_path: None,
        })
        .await
        .expect_err("another task's archive must not be served");

    let BackgroundOutputError::Archived(error) = &error else {
        panic!("a refused archive must be reported as an archive fault: {error}");
    };
    assert!(
        error.to_string().contains("outside the task's workspace"),
        "the refusal must name the containment boundary: {error}"
    );
    Ok(())
}

/// The archive the store resolves for the record's own session is served.
///
/// This is the positive control for the refusal above: the same fixture, with
/// the record naming its own archive, still reads through.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn the_archive_resolved_for_the_records_own_session_is_served() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let own_archive = write_archive_lines(&sessions_dir.join("own-child.json"), &["own line"])?;
    write_archive_lines(&sessions_dir.join("other-child.json"), &["another task's line"])?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: "absent-session",
            session_id: "own-child",
            transcript_path: Some(&own_archive),
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::Archived("own line".to_string()));
    Ok(())
}
