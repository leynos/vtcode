//! Archive attachment for background launches.
//!
//! Declared by `run_tests/mod.rs`. Pins that an archive is attached only to the
//! launch that named its session, using the shared fixtures from
//! `controller_background_ops/tests/support.rs`.
use super::*;

/// Archive paths attach only to the launch that named the resolved session.
#[tokio::test]
async fn archive_paths_attach_only_to_the_launch_that_named_the_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    let archive = temp.path().join("archive.json");

    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-replaced", "exec-launch", &archive);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert!(
            record.archive_path.is_none(),
            "an archive resolved for a replaced launch must not attach to the replacement"
        );
        assert!(record.transcript_path.is_none());
    }

    let (session_id, exec_session_id) = {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        (record.session_id.clone(), record.exec_session_id.clone())
    };
    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", &session_id, &exec_session_id, &archive);
    }
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.archive_path, Some(archive.clone()));
    assert_eq!(record.transcript_path, Some(archive.clone()));
    drop(state);

    controller.signal_shutdown().await;
}

/// A replacement launch with the same session id never inherits the archive.
///
/// The session id is derived from a millisecond timestamp, so a launch that
/// replaces another in the same millisecond can repeat it. The exec session
/// identifies the launch, so attachment requires both to match: otherwise the
/// earlier run's archive is written over the replacement, which then reports
/// output it never produced.
#[tokio::test]
async fn a_repeated_session_id_does_not_attach_the_earlier_launch_archive() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let mut first =
        background_record("background-worker", None, "worker", "exec-first", BackgroundSubprocessStatus::Running, true);
    first.session_id = "session-shared".to_string();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(first.id.clone(), first);
    }

    // A second launch of the same record started in the same millisecond, so
    // it derived the same session id but names its own exec session.
    let mut replacement = background_record(
        "background-worker",
        None,
        "worker",
        "exec-second",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    replacement.session_id = "session-shared".to_string();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(replacement.id.clone(), replacement);
    }

    let archive = temp.path().join("earlier-run.json");
    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-shared", "exec-first", &archive);
    }

    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(record.exec_session_id, "exec-second", "the replacement owns the record");
        assert!(record.archive_path.is_none(), "the earlier launch's archive must not attach to its replacement");
        assert!(record.transcript_path.is_none());
    }

    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-shared", "exec-second", &archive);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.archive_path,
            Some(archive.clone()),
            "the launch that named the exec session records its own archive"
        );
    }

    controller.signal_shutdown().await;
}
