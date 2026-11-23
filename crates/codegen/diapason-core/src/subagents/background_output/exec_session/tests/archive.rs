//! Archive-authorisation, PTY-fallback and classifier contracts for output reads.
//!
//! Declared by `exec_session/tests/mod.rs`. These cases pin the launch-pairing
//! archive rule (a record's exec-session id must embed its session id), the
//! store-authorised archive rule (a recorded path is served only when it
//! resolves to the task's own session), the PTY-handle fallback and the shared
//! missing-session classifier; they build their managers with the fixtures in
//! `exec_session/tests/fixtures.rs`.
use super::super::super::{BackgroundOutput, BackgroundOutputError, BackgroundOutputSource};
#[cfg(feature = "tui")]
use super::fixtures::close_pty_handle;
use super::fixtures::{
    SessionDirOverride, launched_exec_session_id, reader_environment, write_archive, write_archive_lines,
    write_stored_archive,
};
use crate::tools::exec_session::{ExecSessionNotFound, controller_exec_session_id, is_missing_exec_session};
use crate::utils::path::canonicalize_workspace;
use crate::utils::session_archive::override_sessions_dir_for_tests;
use hashbrown::HashMap;
use tempfile::tempdir;

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
    let exec_session_id = launched_exec_session_id("broken-session");
    let archive_path = write_archive(&root)?;

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id(identifier);
    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
            session_id: identifier,
            transcript_path: None,
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::Archived("first line\nsecond line".to_string()));
    Ok(())
}

/// A settled record restored from the ledger still serves its own archive.
///
/// The record's identifiers are stored data, but they were written by a launch
/// and still agree with each other. The process reading them need not be the
/// process that launched the task, so a resumed session can read the archive of
/// a task that settled before a restart. Nothing re-registers the row: the pair
/// it carries is the anchor.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn a_settled_record_restored_from_the_ledger_still_serves_its_own_archive() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    write_stored_archive(&sessions_dir, "settled-child")?;
    // The launch ran in an earlier process; its exec id carries that process's
    // pid, not this one's.
    let exec_session_id = controller_exec_session_id("settled-child", std::process::id().wrapping_add(1), 9);

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
            session_id: "settled-child",
            transcript_path: None,
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(
        output,
        BackgroundOutput::Archived("first line\nsecond line".to_string()),
        "a restored row must keep reading the archive its launch wrote"
    );
    Ok(())
}

/// An unreadable archive is reported as an archive fault.
#[tokio::test]
async fn an_unreadable_archive_is_an_archive_fault() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    let exec_session_id = launched_exec_session_id("unreadable-archive-child");
    let archive_path = root.join("missing-session.json");

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id("absent-archive-child");
    let absent = root.join("absent-archive.json");

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id("pty-output-child");
    write_stored_archive(&sessions_dir, "pty-output-child")?;
    close_pty_handle(&manager, &pty_sessions, &root, exec_session_id.as_str()).await?;
    // Prove the fixture recreates the race the fallback exists for: the record
    // survives and its handle is gone.
    let live = manager.read_session_output(exec_session_id.as_str(), false).await;
    assert!(
        matches!(&live, Err(error) if is_missing_exec_session(error)),
        "the fixture must remove the PTY handle behind the record: {live:?}"
    );

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id("pty-output-child");
    close_pty_handle(&manager, &pty_sessions, &root, exec_session_id.as_str()).await?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id("foreign-archive-child");
    let foreign_archive = write_archive(elsewhere.path())?;

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id("own-child");
    write_stored_archive(&sessions_dir, "own-child")?;
    let foreign_archive = write_archive_lines(&sessions_dir.join("other-child.json"), &["another task's line"])?;
    assert!(
        foreign_archive.starts_with(root.as_path()),
        "the fixture must place the foreign archive inside the workspace"
    );

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
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
    let exec_session_id = launched_exec_session_id("own-child");
    let own_archive = write_archive_lines(&sessions_dir.join("own-child.json"), &["own line"])?;
    write_archive_lines(&sessions_dir.join("other-child.json"), &["another task's line"])?;

    let output = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
            session_id: "own-child",
            transcript_path: Some(&own_archive),
            archive_path: None,
        })
        .await
        .map_err(anyhow::Error::new)?;

    assert_eq!(output, BackgroundOutput::Archived("own line".to_string()));
    Ok(())
}

/// A record whose exec session does not embed its session is refused.
///
/// The two identifiers are stored data once the ledger is restored, so a row
/// can name any session in the store while carrying an exec id a launch built
/// for a different task. Resolving the named session would return that task's
/// transcript when the row has no recorded path, so the pair must agree with
/// the launch convention before the lookup runs. The named victim's archive
/// exists and resolves; the row is refused all the same.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn a_record_whose_exec_session_does_not_embed_its_session_is_refused() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (_manager, _pty_sessions, reader) = reader_environment(&root);
    write_archive_lines(&sessions_dir.join("victim-child.json"), &["another task's line"])?;
    let exec_session_id = launched_exec_session_id("other-child");

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
            session_id: "victim-child",
            transcript_path: None,
            archive_path: None,
        })
        .await
        .expect_err("a record must not resolve a session its exec session does not embed");

    let BackgroundOutputError::Archived(error) = &error else {
        panic!("an unpaired record must be reported as an archive fault: {error}");
    };
    assert!(error.to_string().contains("does not embed"), "the refusal must name the missing pairing: {error}");
    Ok(())
}

/// A caller-chosen `--session-id` authorises nothing.
///
/// The controller passes its task session to its own launches as
/// `--session-id`, but an ordinary exec invocation can pass the same flag. The
/// reader authorises from the record's own pair of identifiers and never from a
/// command, so naming another task's session on an unrelated exec session does
/// not open that task's archive.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn a_caller_chosen_session_id_flag_does_not_authorise_an_archive_read() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionDirOverride;
    let root = canonicalize_workspace(temp.path());
    let (manager, _pty_sessions, reader) = reader_environment(&root);
    write_stored_archive(&sessions_dir, "victim-child")?;
    // The exec id is a launch-shaped one that embeds another task, and the
    // command names the victim session on the command line. The flag used to
    // record the named session as launched; the record's pair is what decides,
    // and this pair does not agree.
    let exec_session_id = launched_exec_session_id("other-child");
    manager
        .create_pipe_session(
            exec_session_id.clone().into(),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 5".to_string(),
                "--session-id".to_string(),
                "victim-child".to_string(),
            ],
            root,
            HashMap::new(),
        )
        .await?;

    let error = reader
        .read_source(BackgroundOutputSource {
            exec_session_id: exec_session_id.as_str(),
            session_id: "victim-child",
            transcript_path: None,
            archive_path: None,
        })
        .await
        .expect_err("a command-line session id must not authorise an archive read");

    manager.close_session(exec_session_id.as_str()).await?;

    let BackgroundOutputError::Archived(error) = &error else {
        panic!("an unpaired record must be reported as an archive fault: {error}");
    };
    assert!(error.to_string().contains("does not embed"), "the refusal must name the missing pairing: {error}");
    Ok(())
}
