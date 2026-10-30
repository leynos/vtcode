//! Stale exec-session continuations map to typed, non-retryable errors.

use super::*;

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

    for args in modes {
        let policy = ExecutionPolicySnapshot::default()
            .with_prevalidated(true)
            .with_safety_prevalidated(true);
        let outcome = registry
            .execute_public_tool_request(ToolExecutionRequest::new(tools::WRITE_STDIN, args).with_policy(policy))
            .await;
        let error = outcome.error.expect("stale continuation should return a structured error");

        let observed = (
            error.category,
            error.error_type,
            error.retryable,
            error.circuit_breaker_impact,
            error.partial_state_possible,
            error
                .details
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

    Ok(())
}
