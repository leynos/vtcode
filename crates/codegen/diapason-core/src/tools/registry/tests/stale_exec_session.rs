//! Stale exec-session continuations map to typed, non-retryable errors.

use super::*;
use crate::metrics::ToolRecoveryEvent;
use crate::tools::exec_session::{ExecSessionLostAfterInput, ExecSessionNotFound, ExecSessionWriterClosed};

/// Loses the session after its input was consumed but before the response is collected.
///
/// `cat` echoes what it consumes, so its next activity signal proves the write
/// was delivered before the session is closed underneath the tool call.
#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn write_stdin_losing_session_after_delivery_reports_partial_state(
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
        .await?;
    let session_id = start
        .get("process_id")
        .and_then(Value::as_str)
        .or_else(|| start.get("session_id").and_then(Value::as_str))
        .ok_or_else(|| anyhow::anyhow!("expected an execution session ID"))?
        .to_owned();
    // The start call must not itself count as a stale continuation; take the
    // baseline only after it has resolved a live session.
    let stale_baseline = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    let session_manager = registry.exec_session_manager();
    let mut activity_rx = session_manager
        .activity_receiver(&session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pipe session should expose an activity receiver"))?;

    let policy = ExecutionPolicySnapshot::default()
        .with_prevalidated(true)
        .with_safety_prevalidated(true);
    let write = registry.execute_public_tool_request(
        ToolExecutionRequest::new(
            tools::WRITE_STDIN,
            json!({
                "session_id": session_id.as_str(),
                "chars": "delivered\n",
                "yield_time_ms": 1_500,
            }),
        )
        .with_policy(policy),
    );
    let close_after_delivery = async {
        tokio::time::timeout(Duration::from_secs(5), activity_rx.changed())
            .await
            .map_err(|error| {
                anyhow::anyhow!("write_stdin did not deliver its input before the test deadline: {error}")
            })??;
        session_manager.close_session(&session_id).await?;
        Ok::<(), anyhow::Error>(())
    };

    let (outcome, closed) = tokio::join!(write, close_after_delivery);
    closed.expect("the session should close after the input was delivered");
    let error = outcome
        .error
        .ok_or_else(|| anyhow::anyhow!("losing the session after the write must fail"))?;

    assert_eq!(error.category, ErrorCategory::ResourceNotFound);
    assert_eq!(error.error_type, ToolErrorType::ResourceNotFound);
    assert!(!error.retryable);
    assert!(error.is_recoverable);
    assert!(!error.circuit_breaker_impact);
    assert!(
        error.partial_state_possible,
        "input delivered before the session was lost may already have side effects"
    );
    assert!(!error.rollback_performed);

    let details = error
        .rendered_details()
        .ok_or_else(|| anyhow::anyhow!("session loss must carry details"))?;
    assert_eq!(details.get("reason").and_then(Value::as_str), Some("stale_exec_session"));
    assert_eq!(details.get("session_id").and_then(Value::as_str), Some(session_id.as_str()));
    assert_eq!(
        details.get("input_delivered").and_then(Value::as_bool),
        Some(true),
        "the post-write classification must report that the input was delivered"
    );

    let message = error.user_message();
    assert!(message.contains("Partial changes may still exist"), "unexpected message: {message}");
    assert!(message.contains("Start a new exec_command"), "unexpected message: {message}");

    let stale_after = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    assert_eq!(
        stale_after,
        stale_baseline + 1,
        "losing the session after input delivery must record exactly one stale write_stdin request"
    );

    Ok(())
}

/// Rejects input to a pipe session whose writer is already closed.
///
/// The session record still exists, so this is not a missing session; the
/// process has exited or its writer task has stopped before any input was
/// delivered. The failure must be a stale session with no partial state and
/// must record one `write_stdin` recovery event.
#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn write_stdin_closed_writer_before_delivery_is_stale_without_partial_state(
    #[future] command_session_fixture: Result<CommandSessionFixture>,
) -> Result<()> {
    let fixture = command_session_fixture.await?;
    let registry = &fixture.registry;
    registry.allow_all_tools().await?;

    let session_id = "run-closed-writer";
    let session_manager = registry.exec_session_manager();
    // Create the session directly so it is not pruned when the process exits;
    // the continuation then resolves a live record whose process has already
    // exited and whose writer can no longer accept input.
    session_manager
        .create_pipe_session(
            session_id.to_string().into(),
            vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
            registry.workspace_root().clone(),
            hashbrown::HashMap::new(),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session_manager.is_session_completed(session_id).await?.is_some() {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let stale_baseline = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);

    let policy = ExecutionPolicySnapshot::default()
        .with_prevalidated(true)
        .with_safety_prevalidated(true);
    let outcome = registry
        .execute_public_tool_request(
            ToolExecutionRequest::new(
                tools::WRITE_STDIN,
                json!({
                    "session_id": session_id,
                    "chars": "input",
                }),
            )
            .with_policy(policy),
        )
        .await;
    let error = outcome.error.expect("a closed writer should return a structured error");

    assert_eq!(error.category, ErrorCategory::ResourceNotFound);
    assert_eq!(error.error_type, ToolErrorType::ResourceNotFound);
    assert!(!error.retryable);
    assert!(error.is_recoverable);
    assert!(!error.circuit_breaker_impact);
    assert!(!error.partial_state_possible, "nothing was delivered, so no partial state is possible");

    let details = error
        .rendered_details()
        .ok_or_else(|| anyhow::anyhow!("a closed writer must carry details"))?;
    assert_eq!(details.get("reason").and_then(Value::as_str), Some("stale_exec_session"));
    assert_eq!(details.get("session_id").and_then(Value::as_str), Some(session_id));
    assert!(
        details.get("input_delivered").is_none(),
        "a pre-write closed writer must not report that input was delivered"
    );

    let stale_after = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    assert_eq!(
        stale_after,
        stale_baseline + 1,
        "a closed writer on write_stdin must record exactly one stale request"
    );

    Ok(())
}

/// Keeps a unified `exec_command` closed-writer continuation out of the
/// `write_stdin` counter.
#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn closed_writer_unified_exec_continuation_does_not_count_as_write_stdin(
    #[future] command_session_fixture: Result<CommandSessionFixture>,
) -> Result<()> {
    let fixture = command_session_fixture.await?;
    let registry = &fixture.registry;
    registry.allow_all_tools().await?;

    let session_id = "run-closed-writer-unified";
    let session_manager = registry.exec_session_manager();
    session_manager
        .create_pipe_session(
            session_id.to_string().into(),
            vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
            registry.workspace_root().clone(),
            hashbrown::HashMap::new(),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session_manager.is_session_completed(session_id).await?.is_some() {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let stale_baseline = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);

    let error = registry
        .execute_command_session_internal(
            json!({"action": "write", "session_id": session_id, "chars": "input"}),
            ExecSettlementMode::Manual,
        )
        .await
        .expect_err("a stale unified continuation should return an error");
    assert!(
        error.chain().any(|cause| cause.is::<ExecSessionWriterClosed>()),
        "a closed writer must surface the typed cause: {error:#}"
    );
    assert!(
        !error.chain().any(|cause| cause.is::<ExecSessionLostAfterInput>()),
        "a pre-write closed writer must not claim partial state: {error:#}"
    );

    let stale_after = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    assert_eq!(
        stale_after, stale_baseline,
        "unified exec_command continuations must not increment the write_stdin counter"
    );

    Ok(())
}

/// Classifies a post-delivery write failure as input-delivered so partial
/// state stays possible.
#[test]
fn closed_writer_after_delivery_reports_partial_state() -> Result<()> {
    let source = anyhow::Error::new(ExecSessionLostAfterInput::new("run-lost"));
    let error = ToolExecutionError::from_anyhow(tools::WRITE_STDIN, &source, 0, false, false, None)
        .with_tool_call_context(tools::WRITE_STDIN, &json!({"session_id": "run-lost", "chars": ""}));

    assert!(error.partial_state_possible, "input delivered before the writer closed may have side effects");
    let details = error
        .rendered_details()
        .ok_or_else(|| anyhow::anyhow!("a post-delivery failure must carry details"))?;
    assert_eq!(details.get("reason").and_then(Value::as_str), Some("stale_exec_session"));
    assert_eq!(details.get("input_delivered").and_then(Value::as_bool), Some(true));
    Ok(())
}

/// Maps input, poll, and wait modes of a stale session to non-partial-state
/// errors.
#[rstest]
#[tokio::test]
async fn stale_write_stdin_modes_are_resource_not_found_without_partial_state(
    #[future] command_session_fixture: Result<CommandSessionFixture>,
) -> Result<()> {
    let fixture_res = command_session_fixture.await;
    let fixture = fixture_res?;
    let registry = &fixture.registry;
    registry.allow_all_tools().await?;
    let modes = [
        json!({"session_id": "run-stale", "chars": "input"}),
        json!({"action": "poll", "session_id": "run-stale"}),
        json!({"action": "wait", "session_id": "run-stale", "yield_time_ms": 1}),
    ];
    let stale_baseline = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);

    for args in modes {
        let policy = ExecutionPolicySnapshot::default()
            .with_prevalidated(true)
            .with_safety_prevalidated(true);
        let outcome = registry
            .execute_public_tool_request(ToolExecutionRequest::new(tools::WRITE_STDIN, args).with_policy(policy))
            .await;
        let error = outcome.error.expect("stale continuation should return a structured error");

        let rendered_details = error.rendered_details();
        let observed = (
            error.category,
            error.error_type,
            error.retryable,
            error.circuit_breaker_impact,
            error.partial_state_possible,
            rendered_details
                .as_ref()
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str),
        );
        assert_eq!(
            observed,
            (
                ErrorCategory::ResourceNotFound,
                ToolErrorType::ResourceNotFound,
                false,
                false,
                false,
                Some("stale_exec_session"),
            ),
            "a stale continuation must be a terminal resource-not-found error with no partial state"
        );
        assert!(
            !error.user_message().contains("Partial changes may still exist"),
            "a stale continuation must not claim that partial changes may exist"
        );
    }

    let stale_after = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    assert_eq!(
        stale_after,
        stale_baseline + 3,
        "each stale write_stdin mode (input, poll, wait) must record one stale request"
    );

    Ok(())
}

/// Keeps unified `exec_command` continuations out of the `write_stdin` counter.
///
/// `exec_command` reaches the same session resolver as `write_stdin`, but the
/// event vocabulary defines [`ToolRecoveryEvent::StaleWriteStdinRequest`] for
/// the latter only, so the shared resolver must not inflate it.
#[rstest]
#[tokio::test]
async fn stale_unified_exec_continuations_do_not_count_as_write_stdin(
    #[future] command_session_fixture: Result<CommandSessionFixture>,
) -> Result<()> {
    let fixture = command_session_fixture.await?;
    let registry = &fixture.registry;
    registry.allow_all_tools().await?;
    let stale_baseline = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    let modes = [
        json!({"action": "write", "session_id": "run-stale", "chars": "input"}),
        json!({"action": "poll", "session_id": "run-stale"}),
        json!({"action": "wait", "session_id": "run-stale", "wait_timeout_seconds": 1}),
    ];

    for args in modes {
        // `exec_command` is the run-only public alias, so unified continuations
        // are dispatched internally with the model-hidden tool identity. Exercise
        // that path directly; the public envelope mapping is covered by the
        // `write_stdin` mode test above.
        let error = registry
            .execute_command_session_internal(args, ExecSettlementMode::Manual)
            .await
            .expect_err("a stale unified continuation should return an error");
        assert!(
            error.chain().any(|cause| cause.is::<ExecSessionNotFound>()),
            "a stale unified continuation must surface the typed missing-session error: {error:#}"
        );
        assert!(
            !error.chain().any(|cause| cause.is::<ExecSessionLostAfterInput>()),
            "a pre-write stale unified continuation must not claim partial state: {error:#}"
        );
    }

    let stale_after = registry
        .metrics_collector()
        .get_tool_recovery_metrics()
        .count(ToolRecoveryEvent::StaleWriteStdinRequest);
    assert_eq!(
        stale_after, stale_baseline,
        "unified exec_command continuations must not increment the write_stdin counter"
    );

    Ok(())
}
