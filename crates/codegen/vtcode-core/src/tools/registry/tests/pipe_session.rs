//! Unix pipe-session coverage for public command and stdin tools.

use super::*;
use crate::tools::exec_session::ExecSessionManager;
use tokio::sync::watch;

/// Holds the crate-visible pipe activity signal beside the session being observed.
#[cfg(unix)]
struct PipeSessionOutputObserver {
    session_manager: ExecSessionManager,
    session_id: String,
    activity_rx: watch::Receiver<u64>,
}

/// Drains buffered output before awaiting the next pipe activity signal.
#[cfg(unix)]
async fn append_pipe_session_output_until_contains(
    observer: &mut PipeSessionOutputObserver,
    observed_output: &mut String,
    expected_output: &str,
) -> Result<()> {
    tokio::time::timeout(HARNESS_COMPLETION_TIMEOUT, async {
        loop {
            if let Some(pending_output) =
                observer.session_manager.read_session_output(&observer.session_id, true).await?
            {
                observed_output.push_str(&pending_output);
            }
            if observed_output.contains(expected_output) {
                return Ok(());
            }

            observer
                .activity_rx
                .changed()
                .await
                .context("pipe session activity receiver closed before expected output")?;
        }
    })
    .await
    .with_context(|| {
        format!("pipe session '{}' did not produce expected output before the test deadline", observer.session_id)
    })?
}

/// Verifies that `write_stdin` continues a live pipe session and returns its echoed input.
#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn test_exec_command_write_stdin_continues_session(
    #[future] command_session_fixture: Result<CommandSessionFixture>,
) -> Result<()> {
    let fixture = command_session_fixture.await?;
    let registry = &fixture.registry;
    registry.allow_all_tools().await?;

    let start = registry
        .execute_tool(
            "exec_command",
            json!({
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        )
        .await
        .context("start public exec command session")?;
    let session_id = start
        .get("process_id")
        .and_then(Value::as_str)
        .or_else(|| start.get("session_id").and_then(Value::as_str))
        .context("expected an execution session ID")?
        .to_owned();
    let session_manager = registry.exec_session_manager();
    let activity_rx = session_manager
        .activity_receiver(&session_id)
        .await?
        .context("pipe session should expose an activity receiver")?;
    let mut observer = PipeSessionOutputObserver {
        session_manager,
        session_id: session_id.clone(),
        activity_rx,
    };

    let write = registry
        .execute_tool(
            "write_stdin",
            json!({
                "session_id": session_id.as_str(),
                "chars": "  keep  \n",
                "yield_time_ms": 250,
            }),
        )
        .await
        .context("continue public exec command session")?;

    assert_eq!(write["success"], true, "write_stdin should succeed: {write:?}");
    assert_eq!(
        write["session_id"].as_str(),
        Some(session_id.as_str()),
        "write_stdin should preserve the session ID: {write:?}"
    );
    assert_eq!(write["is_exited"].as_bool(), Some(false), "write_stdin should leave cat running: {write:?}");

    let mut observed_output = start["output"].as_str().unwrap_or_default().to_owned();
    observed_output.push_str(write["output"].as_str().unwrap_or_default());
    if !observed_output.contains("  keep  \n") {
        append_pipe_session_output_until_contains(&mut observer, &mut observed_output, "  keep  \n").await?;
    }
    assert!(observed_output.contains("  keep  \n"), "combined output was: {observed_output:?}");

    let _closed = registry
        .execute_tool(
            "close_pty_session",
            json!({
                "session_id": session_id,
            }),
        )
        .await
        .context("close public exec command session")?;

    Ok(())
}
