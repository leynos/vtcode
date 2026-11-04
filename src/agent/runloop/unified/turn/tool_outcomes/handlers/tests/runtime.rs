#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use std::time::{Duration, Instant};

use super::*;
use crate::agent::runloop::unified::tool_pipeline::{ToolExecutionStatus, ToolPipelineOutcome};
use crate::agent::runloop::unified::turn::tool_outcomes::execution_result::handle_tool_execution_result;
use diapason_core::config::ToolDisplayMode;
use diapason_core::tools::registry::ToolExecutionError;

#[tokio::test]
async fn blocked_tool_call_guard_emits_tool_and_system_messages() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let max_streak = max_consecutive_blocked_tool_calls_per_turn(&ctx);
    let args = json!({"path":"src/main.rs"});

    let mut outcome = None;
    for idx in 0..=max_streak {
        outcome = enforce_blocked_tool_call_guard(&mut ctx, &format!("blocked_{idx}"), tool_names::READ_FILE, &args);
    }

    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
    assert!(ctx.harness_state.blocked_tool_recovery_pending());
    flush_blocked_tool_recovery(&mut ctx);
    assert!(!ctx.harness_state.blocked_tool_recovery_pending());
    assert!(ctx.harness_state.recovery_is_tool_free());
    assert!(
        ctx.working_history
            .iter()
            .any(|message| message.content.as_text().contains("blocked_streak"))
    );
    assert!(ctx.working_history.iter().any(|message| {
        message.role == uni::MessageRole::System
            && message
                .content
                .as_text()
                .contains("A bounded recovery response will run without more tool calls")
    }));

    // UI synchronization: guard trips must clear any inline loading placeholder
    // so the bottom-line doesn't remain stuck. Input status left/right should
    // be None after the guard fires.
    assert!(ctx.input_status_state.left.is_none() && ctx.input_status_state.right.is_none());
}

#[tokio::test]
async fn blocked_tool_call_guard_allows_configured_consecutive_cap() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let max_streak = max_consecutive_blocked_tool_calls_per_turn(&ctx);
    let args = json!({"path":"src/main.rs"});

    for idx in 0..max_streak {
        let outcome =
            enforce_blocked_tool_call_guard(&mut ctx, &format!("blocked_{idx}"), tool_names::READ_FILE, &args);
        assert!(outcome.is_none(), "blocked call {idx} should stay under cap");
    }

    let outcome = enforce_blocked_tool_call_guard(&mut ctx, "blocked_over_cap", tool_names::READ_FILE, &args);
    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
}

#[tokio::test]
async fn blocked_tool_call_guard_caps_non_consecutive_total_churn() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let limits = blocked_tool_call_limits(&ctx);
    let args = json!({"path":"src/main.rs"});

    for idx in 0..limits.total_cap {
        let outcome =
            enforce_blocked_tool_call_guard(&mut ctx, &format!("blocked_{idx}"), tool_names::READ_FILE, &args);
        assert!(outcome.is_none(), "blocked total {idx} should stay under cap");
        ctx.reset_blocked_tool_call_streak();
    }

    let outcome = enforce_blocked_tool_call_guard(&mut ctx, "blocked_total_over_cap", tool_names::READ_FILE, &args);
    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
    flush_blocked_tool_recovery(&mut ctx);
    assert!(
        ctx.working_history
            .iter()
            .any(|message| message.content.as_text().contains("blocked_total"))
    );
    assert!(ctx.working_history.iter().any(|message| {
        message
            .content
            .as_text()
            .contains(&format!("{} total blocked calls", limits.total_cap))
    }));
}

#[tokio::test]
async fn blocked_tool_call_guard_allows_four_times_total_churn_in_planning_mode() {
    let mut backing = TestContextBacking::new(4).await;
    backing.tool_registry.enable_planning();
    let mut ctx = backing.turn_processing_context();
    let limits = blocked_tool_call_limits(&ctx);
    let args = json!({"path":"src/main.rs"});

    assert_eq!(limits.total_cap, limits.consecutive_cap * 4);
    for idx in 0..limits.total_cap {
        let outcome =
            enforce_blocked_tool_call_guard(&mut ctx, &format!("blocked_{idx}"), tool_names::READ_FILE, &args);
        assert!(outcome.is_none(), "blocked total {idx} should stay under planning cap");
        ctx.reset_blocked_tool_call_streak();
    }

    let outcome = enforce_blocked_tool_call_guard(&mut ctx, "blocked_total_over_cap", tool_names::READ_FILE, &args);
    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
    flush_blocked_tool_recovery(&mut ctx);
    assert!(ctx.working_history.iter().any(|message| {
        message
            .content
            .as_text()
            .contains(&format!("{} total blocked calls", limits.total_cap))
    }));
}

#[tokio::test]
async fn denied_execution_result_uses_planning_total_fuse() {
    let mut backing = TestContextBacking::new(4).await;
    backing.tool_registry.enable_planning();
    let mut ctx = backing.turn_processing_context();
    let limits = blocked_tool_call_limits(&ctx);
    let args = json!({"path": "src/main.rs"});
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    for index in 0..limits.total_cap {
        let pipeline_outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: ToolExecutionError::policy_violation("read_file", "denied for test"),
        });
        let outcome = handle_tool_execution_result(
            &mut outcome_ctx,
            format!("denied_{index}"),
            tool_names::READ_FILE,
            &args,
            &pipeline_outcome,
            Instant::now(),
        )
        .await
        .expect("denied execution result should be handled");
        assert!(outcome.is_none(), "planning denial {index} should stay under total fuse");
        outcome_ctx.ctx.reset_blocked_tool_call_streak();
    }

    let pipeline_outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
        error: ToolExecutionError::policy_violation("read_file", "denied for test"),
    });
    let outcome = handle_tool_execution_result(
        &mut outcome_ctx,
        "denied_over_cap".to_string(),
        tool_names::READ_FILE,
        &args,
        &pipeline_outcome,
        Instant::now(),
    )
    .await
    .expect("total-fuse denial should be handled");

    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
    assert!(outcome_ctx.ctx.harness_state.blocked_tool_recovery_pending());
}

async fn successful_tool_history_for_display_mode(mode: ToolDisplayMode) -> Vec<(Option<String>, String)> {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    ctx.renderer.set_tool_display_mode(mode);

    let output = json!({
        "command": "printf context",
        "output": "stdout context",
        "stdout": "stdout context",
        "stderr": "stderr context",
        "exit_code": 0,
        "critical_note": "critical context",
        "next_action": "next context",
        "generated_files": {
            "files": ["src/generated.rs"]
        },
        "metadata_flag": false,
        "metadata_count": 0,
        "fallback_tool": "read_file",
        "fallback_tool_args": {"path": "src/generated.rs"}
    });
    let pipeline_outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output,
        stdout: Some("stdout context".to_string()),
        modified_files: Vec::new(),
        command_success: true,
    });
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    {
        let mut outcome_ctx = ToolOutcomeContext {
            ctx: &mut ctx,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
        };

        handle_tool_execution_result(
            &mut outcome_ctx,
            "context_call".to_string(),
            tool_names::EXECUTE_CODE,
            &json!({"command": "printf context"}),
            &pipeline_outcome,
            Instant::now(),
        )
        .await
        .expect("successful command result should be handled");
    }

    ctx.working_history
        .iter()
        .filter(|message| message.role == uni::MessageRole::Tool)
        .map(|message| (message.tool_call_id.clone(), message.content.as_text().into_owned()))
        .collect()
}

#[tokio::test]
async fn compact_display_keeps_provider_history_identical_to_expanded_display() {
    let compact_history = successful_tool_history_for_display_mode(ToolDisplayMode::Compact).await;
    let expanded_history = successful_tool_history_for_display_mode(ToolDisplayMode::Expanded).await;

    assert_eq!(compact_history, expanded_history);
    assert_eq!(compact_history.len(), 1);
    let content = &compact_history[0].1;
    for expected in [
        "stdout context",
        "stderr context",
        "critical context",
        "next context",
        "src/generated.rs",
        "metadata_flag",
        "metadata_count",
        "read_file",
        "fallback_tool_args",
    ] {
        assert!(content.contains(expected), "provider history lost {expected:?}: {content}");
    }
    assert!(!content.contains("Ctrl+T"));
    assert!(!content.contains("click to expand"));
}

#[tokio::test]
async fn non_zero_read_result_is_diagnosed_without_successful_readonly_signature() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let args = json!({"path": "missing.rs"});
    let output = json!({
        "path": "missing.rs",
        "stderr": "No such file or directory",
        "exit_code": 1
    });
    let pipeline_outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output,
        stdout: None,
        modified_files: Vec::new(),
        command_success: false,
    });
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    handle_tool_execution_result(
        &mut outcome_ctx,
        "missing-read".to_string(),
        tool_names::READ_FILE,
        &args,
        &pipeline_outcome,
        Instant::now(),
    )
    .await
    .expect("non-zero result should be handled");

    let tool_response = outcome_ctx
        .ctx
        .working_history
        .iter()
        .find(|message| message.role == uni::MessageRole::Tool)
        .expect("tool response should be recorded");
    let response: serde_json::Value =
        serde_json::from_str(&tool_response.content.as_text()).expect("tool response should remain JSON");
    assert_eq!(
        response["diagnosis"]["observed"],
        "'read_file' returned exit code 1. Stderr: No such file or directory"
    );
    assert_eq!(
        outcome_ctx
            .ctx
            .harness_state
            .snapshot_turn_diagnostics(Default::default(), 0)
            .failed_tool_calls,
        1
    );
    let signature =
        crate::agent::runloop::unified::turn::tool_outcomes::helpers::signature_key_for(tool_names::READ_FILE, &args);
    assert!(!outcome_ctx.ctx.harness_state.has_successful_readonly_signature(&signature));
}

#[tokio::test]
async fn blocked_tool_call_guard_short_circuits_to_recovery_when_active() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let args = json!({"path":"src/main.rs"});
    ctx.activate_recovery("loop detector");

    let outcome = enforce_blocked_tool_call_guard(&mut ctx, "blocked_recovery", tool_names::READ_FILE, &args);

    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
}

#[tokio::test]
async fn malformed_tool_calls_trip_preflight_circuit_at_configured_cap() {
    let mut backing = TestContextBacking::new(4).await;
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let malformed_call = || {
        PreparedAssistantToolCall::new(uni::ToolCall::function(
            "malformed_call".to_string(),
            tool_names::CODE_SEARCH.to_string(),
            "{not-json".to_string(),
        ))
    };
    let max_failures = {
        let ctx = backing.turn_processing_context();
        max_consecutive_blocked_tool_calls_per_turn(&ctx)
    };

    for failure in 1..max_failures {
        let mut ctx = backing.turn_processing_context();
        let mut outcome_ctx = ToolOutcomeContext {
            ctx: &mut ctx,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
        };
        let outcome = handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
            .await
            .expect("malformed call should produce recoverable feedback");
        assert!(outcome.is_none(), "failure {failure} should remain recoverable");
    }

    let mut ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };
    let outcome = handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
        .await
        .expect("preflight circuit should return a structured outcome");

    // The circuit breaker no longer hard-blocks; it arms a bounded tool-free
    // recovery pass so the model can synthesize a plain-text response instead
    // of the turn terminating as `Blocked` (which silently dropped approved-
    // plan builds — checkpoint turn_874).
    assert!(
        matches!(outcome, Some(TurnHandlerOutcome::Continue)),
        "circuit trip must return Continue (recovery), not Break(Blocked)"
    );
    assert!(ctx.working_history.iter().any(|message| {
        let content = message.content.as_text();
        content.contains("preflight_circuit_breaker")
            && content.contains("schema_correction")
            && content.contains("next_action")
    }));
    // The flush (called inside handle_tool_calls via dispatch) must arm
    // tool-free recovery and push the synthesis directive.
    assert!(ctx.harness_state.recovery_is_tool_free(), "preflight circuit trip must arm tool-free recovery mode");
    assert!(ctx.harness_state.consume_recovery_pass(), "recovery pass must be pending after the circuit flush");
    assert!(ctx.working_history.iter().any(|message| {
        message.role == uni::MessageRole::System
            && message.content.as_text().contains("circuit breaker")
            && message.content.as_text().contains("tools are disabled for this pass")
    }));
}

#[tokio::test]
async fn valid_admitted_tool_call_resets_preflight_circuit_streak() {
    let mut backing = TestContextBacking::new(4).await;
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let malformed_call = || {
        PreparedAssistantToolCall::new(uni::ToolCall::function(
            "malformed_call".to_string(),
            tool_names::CODE_SEARCH.to_string(),
            "{not-json".to_string(),
        ))
    };

    for _ in 0..2 {
        let mut ctx = backing.turn_processing_context();
        let mut outcome_ctx = ToolOutcomeContext {
            ctx: &mut ctx,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
        };
        assert!(
            handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
                .await
                .expect("malformed call should be recoverable")
                .is_none()
        );
    }

    let valid_file = backing.sample_file.clone();
    let valid_args = json!({"path": valid_file.to_string_lossy()});
    cache_tool_permission(&mut backing, tool_names::READ_FILE, &valid_args, PermissionGrant::Permanent).await;
    let mut ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };
    assert!(
        handle_single_tool_call(&mut outcome_ctx, "valid_call", tool_names::READ_FILE, valid_args)
            .await
            .expect("valid call should execute")
            .is_none()
    );

    let max_failures = {
        let ctx = backing.turn_processing_context();
        max_consecutive_blocked_tool_calls_per_turn(&ctx)
    };
    for failure in 1..max_failures {
        let mut ctx = backing.turn_processing_context();
        let mut outcome_ctx = ToolOutcomeContext {
            ctx: &mut ctx,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
        };
        assert!(
            handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
                .await
                .expect("malformed call after reset should be recoverable")
                .is_none(),
            "failure {failure} should remain recoverable"
        );
    }
    let mut ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };
    let outcome = handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
        .await
        .expect("preflight circuit should return a recovery outcome");
    assert!(
        matches!(outcome, Some(TurnHandlerOutcome::Continue)),
        "circuit trip after reset must return Continue, not Break(Blocked)"
    );
    assert!(ctx.harness_state.recovery_is_tool_free(), "circuit trip after reset must arm tool-free recovery");
}

#[tokio::test]
async fn batched_preflight_failures_trip_the_same_circuit() {
    let mut backing = TestContextBacking::new(4).await;
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let malformed_call = |id: &str| {
        PreparedAssistantToolCall::new(uni::ToolCall::function(
            id.to_string(),
            tool_names::CODE_SEARCH.to_string(),
            r#"{"query":4}"#.to_string(),
        ))
    };
    let max_failures = {
        let ctx = backing.turn_processing_context();
        max_consecutive_blocked_tool_calls_per_turn(&ctx)
    };

    for _ in 0..max_failures {
        let mut ctx = backing.turn_processing_context();
        ctx.full_auto = true;
        let mut outcome_ctx = ToolOutcomeContext {
            ctx: &mut ctx,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
        };
        let calls = [malformed_call("batch_a"), malformed_call("batch_b")];
        let outcome = handle_tool_calls(&mut outcome_ctx, &calls)
            .await
            .expect("batched preflight validation should complete");
        if outcome.is_some() {
            assert!(
                matches!(outcome, Some(TurnHandlerOutcome::Continue)),
                "batched preflight circuit trip must return Continue, not Break(Blocked)"
            );
            return;
        }
    }

    panic!("batched preflight failures did not trip the circuit");
}

#[tokio::test]
async fn preflight_circuit_drains_remaining_batch_tool_responses() {
    let mut backing = TestContextBacking::new(4).await;
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let max_failures = {
        let ctx = backing.turn_processing_context();
        max_consecutive_blocked_tool_calls_per_turn(&ctx)
    };
    let mut tool_calls = Vec::with_capacity(max_failures + 2);
    for index in 0..max_failures {
        tool_calls.push(PreparedAssistantToolCall::new(uni::ToolCall::function(
            format!("malformed_batch_{index}"),
            tool_names::CODE_SEARCH.to_string(),
            "{not-json".to_string(),
        )));
    }
    for index in 0..2 {
        tool_calls.push(PreparedAssistantToolCall::new(uni::ToolCall::function(
            format!("valid_after_{index}"),
            tool_names::CODE_SEARCH.to_string(),
            serde_json::to_string(&json!({"query": format!("batch continuation {index}")}))
                .expect("serialize valid tool args"),
        )));
    }
    let expected_ids = tool_calls.iter().map(|call| call.call_id().to_string()).collect::<Vec<_>>();

    let mut ctx = backing.turn_processing_context();
    ctx.full_auto = true;
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };
    let outcome = handle_tool_calls(&mut outcome_ctx, &tool_calls)
        .await
        .expect("batch preflight circuit should return a recovery outcome");

    // The circuit trip arms recovery (Continue) instead of hard-blocking.
    assert!(
        matches!(outcome, Some(TurnHandlerOutcome::Continue)),
        "batch preflight circuit trip must return Continue, not Break(Blocked)"
    );
    // Every tool-call ID — including the drained remaining batch calls — must
    // receive a tool response so providers that enforce strict
    // assistant/tool ordering do not reject the next request.
    for tool_call_id in expected_ids {
        assert!(
            ctx.working_history
                .iter()
                .any(|message| message.tool_call_id.as_deref() == Some(tool_call_id.as_str())),
            "missing tool response for {tool_call_id}"
        );
    }
    // The flush must arm tool-free recovery after all responses land.
    assert!(
        ctx.harness_state.recovery_is_tool_free(),
        "batch preflight circuit trip must arm tool-free recovery"
    );
}

#[tokio::test]
async fn preflight_circuit_does_not_block_approved_plan_execution() {
    // Regression for checkpoint turn_874: when the preflight circuit breaker
    // tripped during an approved-plan build turn, the old code returned
    // `Break(Blocked)`. A blocked result never derives
    // `plan_approved_execution_pending`, so the session loop silently dropped
    // the approved build and the agent could not continue. The fix arms a
    // bounded tool-free recovery pass instead, returning `Continue`.
    let mut backing = TestContextBacking::new(4).await;
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let malformed_call = || {
        PreparedAssistantToolCall::new(uni::ToolCall::function(
            "malformed_call".to_string(),
            tool_names::CODE_SEARCH.to_string(),
            "{not-json".to_string(),
        ))
    };
    let max_failures = {
        let ctx = backing.turn_processing_context();
        max_consecutive_blocked_tool_calls_per_turn(&ctx)
    };

    // Simulate the fresh build turn that the session loop starts after the
    // user approves the plan.
    backing.harness_state.set_approved_plan_execution(true);
    assert!(backing.harness_state.is_approved_plan_execution());

    for failure in 1..max_failures {
        let mut ctx = backing.turn_processing_context();
        let mut outcome_ctx = ToolOutcomeContext {
            ctx: &mut ctx,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
        };
        let outcome = handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
            .await
            .expect("malformed call should produce recoverable feedback");
        assert!(outcome.is_none(), "failure {failure} should remain recoverable");
    }

    let mut ctx = backing.turn_processing_context();
    // Re-assert the approved-plan flag survived into the turn context.
    assert!(ctx.harness_state.is_approved_plan_execution());
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };
    let outcome = handle_tool_calls(&mut outcome_ctx, &[malformed_call()])
        .await
        .expect("circuit trip should produce a recovery outcome");

    // Must NOT be Break(Blocked) — that would silently drop the approved build.
    assert!(
        matches!(outcome, Some(TurnHandlerOutcome::Continue)),
        "approved-plan build must not hard-block on preflight circuit trip"
    );
    assert!(
        ctx.harness_state.recovery_is_tool_free(),
        "approved-plan build must arm tool-free recovery after circuit trip"
    );
    assert!(
        ctx.harness_state.consume_recovery_pass(),
        "recovery pass must be pending so the next loop iteration runs it"
    );
}

#[tokio::test]
async fn flush_preflight_circuit_recovery_is_idempotent_and_arms_tool_free_mode() {
    // The flush must be safe to call multiple times (dispatch calls it at
    // several exit points) and must push the directive exactly once while
    // arming tool-free recovery — mirroring the budget-exhaustion flush.
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();

    // Arming without flushing must not push the directive.
    ctx.harness_state.arm_preflight_circuit_recovery();
    let directive_count_before = ctx
        .working_history
        .iter()
        .filter(|message| {
            message.role == uni::MessageRole::System
                && message.content.as_text().contains("circuit breaker")
                && message.content.as_text().contains("tools are disabled for this pass")
        })
        .count();
    assert_eq!(directive_count_before, 0, "directive must not be pushed before flush");

    // First flush pushes the directive and arms recovery.
    flush_preflight_circuit_recovery(&mut ctx);
    let directive_count_after = ctx
        .working_history
        .iter()
        .filter(|message| {
            message.role == uni::MessageRole::System
                && message.content.as_text().contains("circuit breaker")
                && message.content.as_text().contains("tools are disabled for this pass")
        })
        .count();
    assert_eq!(directive_count_after, 1, "directive must be pushed exactly once");
    assert!(ctx.harness_state.recovery_is_tool_free(), "flush must arm tool-free recovery mode");
    assert!(ctx.harness_state.consume_recovery_pass(), "recovery pass must be pending after flush");

    // Second flush is a no-op (the pending flag was consumed).
    flush_preflight_circuit_recovery(&mut ctx);
    let directive_count_final = ctx
        .working_history
        .iter()
        .filter(|message| {
            message.role == uni::MessageRole::System
                && message.content.as_text().contains("circuit breaker")
                && message.content.as_text().contains("tools are disabled for this pass")
        })
        .count();
    assert_eq!(directive_count_final, 1, "directive must not be duplicated");
}

#[tokio::test]
async fn unified_validation_ignores_preseeded_legacy_loop_detector_state() {
    let mut backing = TestContextBacking::new(2).await;
    backing.select_build_primary_agent();
    let valid_file = backing.sample_file.clone();
    let valid_args = json!({"path": valid_file.to_string_lossy()});
    cache_tool_permission(&mut backing, tool_names::READ_FILE, &valid_args, PermissionGrant::Permanent).await;

    backing.autonomous_executor.set_loop_limit(tool_names::READ_FILE, 2);
    let seeded_args = json!({"path": valid_file.to_string_lossy()});
    assert!(
        backing
            .autonomous_executor
            .record_tool_call(tool_names::READ_FILE, &seeded_args)
            .is_none()
    );
    let _ = backing
        .autonomous_executor
        .record_tool_call(tool_names::READ_FILE, &seeded_args);
    let warning = backing
        .autonomous_executor
        .record_tool_call(tool_names::READ_FILE, &seeded_args);
    assert!(warning.is_some());
    assert!(backing.autonomous_executor.is_hard_limit_exceeded(tool_names::READ_FILE));

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome =
        handle_single_tool_call(&mut outcome_ctx, "legacy_detector_seeded", tool_names::READ_FILE, valid_args)
            .await
            .expect("unified validation should ignore legacy detector state");

    assert!(outcome.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 1);
    assert!(
        !outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("Loop detector stopped repeated") })
    );
    assert!(backing.autonomous_executor.is_hard_limit_exceeded(tool_names::READ_FILE));
}

#[tokio::test]
async fn active_primary_agent_policy_blocks_hallucinated_denied_tool_call() {
    let mut backing = TestContextBacking::new(2).await;
    let mut spec = test_primary_agent_spec("reader");
    spec.tools = Some(vec![tool_names::READ_FILE.to_string()]);
    spec.disallowed_tools = vec![tool_names::READ_FILE.to_string()];
    backing.select_primary_agent_from_specs(&[spec], "reader");

    let valid_file = backing.sample_file.clone();
    let args = json!({"path": valid_file.to_string_lossy()});
    let mut ctx = backing.turn_processing_context();

    let result = validate_tool_call(&mut ctx, "denied_read", tool_names::READ_FILE, &args)
        .await
        .expect("validation should complete");

    assert!(matches!(result, ValidationResult::Blocked));
    assert!(
        ctx.working_history
            .iter()
            .any(|message| { message.content.as_text().contains("active primary agent policy") })
    );
    assert_eq!(ctx.harness_state.tool_calls, 0);
}

#[tokio::test]
async fn invalid_preflight_arguments_do_not_trip_blocked_tool_guard() {
    let mut backing = TestContextBacking::new(4).await;
    backing.tool_registry.enable_planning();
    let mut ctx = backing.turn_processing_context();

    let result = validate_tool_call(
        &mut ctx,
        "invalid_code_search",
        tool_names::CODE_SEARCH,
        &json!({"query": "session setup", "max_results": 120}),
    )
    .await
    .expect("invalid arguments should be returned as structured tool feedback");

    assert!(matches!(result, ValidationResult::Handled));
    assert_eq!(ctx.harness_state.consecutive_blocked_tool_calls, 0);
    assert!(
        ctx.working_history
            .iter()
            .any(|message| { message.role == uni::MessageRole::Tool && message.content.as_text().contains("max=100") })
    );
}

#[tokio::test]
async fn repeated_shell_guard_activates_recovery_without_breaking_turn() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let max_repeated_runs = ctx
        .vt_cfg
        .map(|cfg| cfg.tools.max_repeated_tool_calls)
        .filter(|value| *value > 0)
        .unwrap_or(diapason_core::config::constants::defaults::DEFAULT_MAX_REPEATED_TOOL_CALLS);
    let args = json!({"action":"run","command":"cargo check"});

    let mut outcome = None;
    for idx in 0..=max_repeated_runs {
        outcome = enforce_repeated_shell_run_guard(&mut ctx, &format!("shell_{idx}"), tool_names::UNIFIED_EXEC, &args);
    }

    assert!(matches!(outcome, Some(ValidationResult::Blocked)));
    assert!(ctx.is_recovery_active());
}

#[tokio::test]
async fn duplicate_task_tracker_create_is_blocked_not_breaking() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();
    let args = json!({
        "action": "create",
        "title": "Task Checklist",
        "items": ["step 1"]
    });

    let first =
        enforce_duplicate_task_tracker_create_guard(&mut ctx, "task_tracker_first", tool_names::TASK_TRACKER, &args);
    assert!(first.is_none());
    let second =
        enforce_duplicate_task_tracker_create_guard(&mut ctx, "task_tracker_second", tool_names::TASK_TRACKER, &args);
    assert!(matches!(second, Some(ValidationResult::Blocked)));
}

#[tokio::test]
async fn validate_tool_call_blocks_when_wall_clock_budget_exhausted() {
    let mut backing = TestContextBacking::new(4).await;
    let sample_path = backing.sample_file.to_string_lossy().to_string();
    let mut ctx = backing.turn_processing_context();
    ctx.harness_state.turn_started_at = Instant::now()
        .checked_sub(Duration::from_secs(ctx.harness_state.max_tool_wall_clock.as_secs() + 1))
        .unwrap();

    let result =
        validate_tool_call(&mut ctx, "wall_clock_exhausted", tool_names::READ_FILE, &json!({"path": sample_path}))
            .await
            .expect("validate wall-clock-exhausted tool call");

    assert!(matches!(result, ValidationResult::Blocked));
    assert!(ctx.working_history.iter().any(|message| {
        message
            .content
            .as_text()
            .contains("Policy violation: exceeded tool wall clock budget")
    }));

    // A second rejected call in the same batch must NOT repeat the full policy
    // message — it gets a compact "call skipped" stub instead.
    let second =
        validate_tool_call(&mut ctx, "wall_clock_exhausted_2", tool_names::READ_FILE, &json!({"path": sample_path}))
            .await
            .expect("validate second wall-clock-exhausted tool call");
    assert!(matches!(second, ValidationResult::Blocked));
    let policy_violation_count = ctx
        .working_history
        .iter()
        .filter(|message| {
            message
                .content
                .as_text()
                .contains("Policy violation: exceeded tool wall clock budget")
        })
        .count();
    assert_eq!(policy_violation_count, 1, "full policy message must be emitted exactly once per turn");
    assert!(
        ctx.working_history
            .iter()
            .any(|message| { message.content.as_text().contains("call skipped") })
    );

    // Flushing after the batch pushes exactly one "synthesize now" directive so
    // the model produces a final answer from gathered context instead of stalling.
    flush_budget_synthesis_directives(&mut ctx);
    let directive_count = ctx
        .working_history
        .iter()
        .filter(|message| {
            message.role == uni::MessageRole::System
                && message.content.as_text().contains("Synthesize your final answer now")
        })
        .count();
    assert_eq!(directive_count, 1, "synthesis directive must be pushed once");

    // The flush must also arm the tool-free recovery pass so the next request
    // strips tool definitions at the API level — the directive alone is
    // advisory and models kept emitting (rejected) tool calls after it
    // (checkpoints turn_637, turn_647).
    assert!(
        ctx.harness_state.recovery_is_tool_free(),
        "wall-clock directive flush must arm tool-free recovery mode"
    );
    assert!(
        ctx.harness_state.consume_recovery_pass(),
        "recovery pass must be pending so the next loop iteration consumes it"
    );

    // Flushing again is a no-op (the pending flag is consumed).
    flush_budget_synthesis_directives(&mut ctx);
    let directive_count_after = ctx
        .working_history
        .iter()
        .filter(|message| {
            message.role == uni::MessageRole::System
                && message.content.as_text().contains("Synthesize your final answer now")
        })
        .count();
    assert_eq!(directive_count_after, 1, "directive must not be duplicated");
}

#[tokio::test]
async fn start_planning_clears_task_tracker_create_signatures() {
    let mut backing = TestContextBacking::new(4).await;
    backing.select_build_primary_agent();
    let enter_args = json!({});
    cache_tool_permission(&mut backing, tool_names::START_PLANNING, &enter_args, PermissionGrant::Permanent).await;

    let mut ctx = backing.turn_processing_context();
    let create_args = json!({
        "action": "create",
        "title": "Task Checklist",
        "items": ["step 1"]
    });
    let first = enforce_duplicate_task_tracker_create_guard(
        &mut ctx,
        "task_tracker_seed",
        tool_names::TASK_TRACKER,
        &create_args,
    );
    assert!(first.is_none());

    let result = validate_tool_call(&mut ctx, "start_planning_call", tool_names::START_PLANNING, &enter_args)
        .await
        .expect("validate start_planning");
    assert!(matches!(result, ValidationResult::Proceed(_)));

    let second = enforce_duplicate_task_tracker_create_guard(
        &mut ctx,
        "task_tracker_after_plan",
        tool_names::TASK_TRACKER,
        &create_args,
    );
    assert!(second.is_none());
}

#[tokio::test]
async fn recovery_skip_step_pushes_structured_tool_message() {
    let mut backing = TestContextBacking::new(4).await;
    let mut ctx = backing.turn_processing_context();

    let outcome = recovery::apply_recovery_action(
        &mut ctx,
        "recovery_call",
        crate::agent::runloop::unified::turn::recovery_flow::RecoveryAction::SkipStep,
    )
    .await
    .expect("skip-step recovery should succeed");

    assert!(matches!(outcome, Some(ValidationResult::Handled)));
    assert!(
        ctx.working_history
            .iter()
            .any(|message| { message.content.as_text().contains("\"skipped\":true") })
    );
}

#[tokio::test]
async fn repeated_identical_readonly_call_in_same_turn_reuses_recent_result() {
    let mut backing = TestContextBacking::new(4).await;
    backing.select_build_primary_agent();
    let args = json!({
        "path": backing.sample_file.to_string_lossy()
    });

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let first = handle_single_tool_call(&mut outcome_ctx, "read_once", tool_names::READ_FILE, args.clone())
        .await
        .expect("first readonly call should succeed");

    assert!(first.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 1);
    assert_eq!(outcome_ctx.ctx.tool_registry.execution_history_len(), 1);
    assert!(outcome_ctx.ctx.harness_state.has_successful_readonly_signature(
        &crate::agent::runloop::unified::turn::tool_outcomes::helpers::signature_key_for(tool_names::READ_FILE, &args,),
    ));
    assert!(
        outcome_ctx
            .ctx
            .tool_registry
            .find_recent_successful_output(
                tool_names::READ_FILE,
                &args,
                outcome_ctx.ctx.harness_state.max_tool_wall_clock
            )
            .is_some()
    );

    let second = handle_single_tool_call(&mut outcome_ctx, "read_twice", tool_names::READ_FILE, args)
        .await
        .expect("duplicate readonly call should be reused");

    assert!(second.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 1);
    assert_eq!(outcome_ctx.ctx.tool_registry.execution_history_len(), 1);
    assert_eq!(
        outcome_ctx
            .ctx
            .harness_state
            .snapshot_turn_diagnostics(Default::default(), 0)
            .reused_results,
        1
    );
    assert!(
        outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("\"reused_recent_result\":true") })
    );
    assert!(
        outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("\"result_ref_only\":true") })
    );
}

#[tokio::test]
async fn repeated_same_file_paginated_reads_do_not_trip_read_family_cap() {
    // Regression: reading different slices of the same file (different
    // offset/limit) is legitimate pagination, not a retry loop. The family
    // cap must NOT trip on these — otherwise the agent is forced into a
    // tool-free recovery pass that produces a garbage final answer.
    // Reproduces the failure seen in checkpoint turn_613.
    let read_family_cap = 4;
    let mut backing = TestContextBacking::new(read_family_cap).await;
    backing.select_build_primary_agent();
    let sample_file = backing.sample_file.clone();
    std::fs::write(&sample_file, (1..=16).map(|idx| format!("line {idx}\n")).collect::<String>())
        .expect("rewrite sample file");
    let sample_path = sample_file.to_string_lossy().to_string();

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let build_paginated_read_args = |line: usize| {
        json!({
            "path": sample_path.clone(),
            "line_start": line,
            "line_end": line
        })
    };

    // Paginated reads: each variant targets a different line range, so each
    // gets a distinct family key (`file_operation::read::<path>::off=<line>`).
    for idx in 1..=read_family_cap {
        let outcome = handle_single_tool_call(
            &mut outcome_ctx,
            &format!("read_variant_{idx}"),
            tool_names::READ_FILE,
            build_paginated_read_args(idx),
        )
        .await
        .expect("paginated read variant should complete");

        assert!(outcome.is_none(), "paginated read {idx} must not be blocked by the family cap");
    }

    // No pagination burst should have tripped recovery: the streak resets on
    // every distinct slice, so it never reaches the cap.
    assert_eq!(
        outcome_ctx.ctx.harness_state.consecutive_same_file_read_family_calls, 1,
        "paginated reads must reset the family streak, not accumulate it"
    );
    assert!(!outcome_ctx.ctx.is_recovery_active(), "recovery must not activate for legitimate pagination");
    assert!(
        !outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("repeated_read_family") }),
        "no repeated_read_family error should be emitted for pagination"
    );
}

#[tokio::test]
async fn repeated_identical_slice_read_trips_read_family_cap() {
    // True retry loop: same path + same slice, repeated verbatim. The cap must
    // trip here — this is the guard's reason for existing.
    let read_family_cap = 4;
    let mut backing = TestContextBacking::new(read_family_cap).await;
    backing.select_build_primary_agent();
    let sample_file = backing.sample_file.clone();
    std::fs::write(&sample_file, (1..=16).map(|idx| format!("line {idx}\n")).collect::<String>())
        .expect("rewrite sample file");
    let sample_path = sample_file.to_string_lossy().to_string();

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    // Identical slice every time -> same family key -> streak accumulates.
    let identical_args = json!({
        "path": sample_path.clone(),
        "offset": 0,
        "limit": 4
    });

    for idx in 1..read_family_cap {
        let outcome = handle_single_tool_call(
            &mut outcome_ctx,
            &format!("read_repeat_{idx}"),
            tool_names::READ_FILE,
            identical_args.clone(),
        )
        .await
        .expect("identical read should complete");
        assert!(outcome.is_none(), "read {idx} below cap should not block");
    }
    assert_eq!(outcome_ctx.ctx.harness_state.consecutive_same_file_read_family_calls, read_family_cap - 1,);

    let execution_history_len_before_block = outcome_ctx.ctx.tool_registry.execution_history_len();
    let tool_calls_before_block = outcome_ctx.ctx.harness_state.tool_calls;

    let blocked =
        handle_single_tool_call(&mut outcome_ctx, "read_repeat_blocked", tool_names::READ_FILE, identical_args.clone())
            .await
            .expect("read-family cap attempt should be handled");

    assert!(matches!(blocked, Some(TurnHandlerOutcome::Continue)));
    assert_eq!(outcome_ctx.ctx.tool_registry.execution_history_len(), execution_history_len_before_block);
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, tool_calls_before_block);
    assert_eq!(outcome_ctx.ctx.harness_state.consecutive_same_file_read_family_calls, read_family_cap);
    assert!(outcome_ctx.ctx.is_recovery_active());
    assert!(
        outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("repeated_read_family") })
    );
}

#[tokio::test]
async fn repeated_paginated_sed_reads_eventually_trip_per_file_path_cap() {
    // turn_911-style regression: simple `sed -n` pagination should behave like
    // file reads. Different ranges must not trip the identical-slice family
    // cap, but repeated exploration of the same file should still hit the
    // shared per-file-path fuse and stop the loop.
    let mut backing = TestContextBacking::new(20).await;
    backing.select_build_primary_agent();
    let sample_file = backing.sample_file.clone();
    std::fs::write(&sample_file, (1..=16).map(|idx| format!("line {idx}\n")).collect::<String>())
        .expect("rewrite sample file");
    let sample_path = sample_file.to_string_lossy().to_string();

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let mut blocked_at = None;
    for idx in 1..=10 {
        let args = json!({
            "cmd": format!("sed -n '{idx},{idx}p' {sample_path}")
        });
        let outcome =
            handle_single_tool_call(&mut outcome_ctx, &format!("sed_read_{idx}"), tool_names::EXEC_COMMAND, args)
                .await
                .expect("sed read should complete");

        if outcome.is_some() {
            blocked_at = Some(idx);
            break;
        }
    }

    assert_eq!(blocked_at, Some(7), "sed pagination should stop on the shared per-file-path cap");
    assert_eq!(
        outcome_ctx.ctx.harness_state.consecutive_same_file_read_family_calls, 1,
        "different sed ranges must reset the identical-slice family streak"
    );
    assert!(outcome_ctx.ctx.is_recovery_active());
    assert!(
        outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("repeated_read_family") }),
        "sed pagination cap should reuse the existing repeated-read guard payload"
    );
}

#[tokio::test]
async fn denied_tool_permission_emits_policy_response_without_budget_burn() {
    let mut backing = TestContextBacking::new(2).await;
    backing.select_build_primary_agent();
    let valid_file = backing.sample_file.clone();
    let denial_args = json!({"path": valid_file.to_string_lossy()});
    cache_tool_permission(&mut backing, tool_names::READ_FILE, &denial_args, PermissionGrant::Denied).await;

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome = handle_single_tool_call(&mut outcome_ctx, "denied", tool_names::READ_FILE, denial_args)
        .await
        .expect("denied permission should be handled");

    assert!(outcome.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 0);
    assert!(
        outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("execution denied by policy") })
    );
}

#[tokio::test]
async fn prepared_tool_calls_respect_unlimited_budget_when_cap_disabled() {
    let mut backing = TestContextBacking::new(0).await;
    backing.select_build_primary_agent();
    let valid_file = backing.sample_file.clone();
    let valid_args = json!({"path": valid_file.to_string_lossy()});
    cache_tool_permission(&mut backing, tool_names::READ_FILE, &valid_args, PermissionGrant::Permanent).await;

    let tool_call = PreparedAssistantToolCall::new(uni::ToolCall::function(
        "prepared_read".to_string(),
        tool_names::READ_FILE.to_string(),
        serde_json::to_string(&valid_args).expect("serialize tool args"),
    ));

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome = handle_prepared_tool_call(&mut outcome_ctx, &tool_call)
        .await
        .expect("prepared tool call should execute");

    assert!(outcome.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 1);
    assert!(!outcome_ctx.ctx.harness_state.tool_budget_exhausted());
    assert!(
        !outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("exceeded max tool calls per turn") })
    );
}

#[tokio::test]
async fn multiple_prepared_tool_calls_respect_unlimited_budget_when_cap_disabled() {
    let mut backing = TestContextBacking::new(0).await;
    backing.select_build_primary_agent();
    let second_file = backing.sample_file.parent().expect("temp workspace root").join("other.txt");
    std::fs::write(&second_file, "world\n").expect("write second sample file");

    let tool_calls = vec![
        PreparedAssistantToolCall::new(uni::ToolCall::function(
            "prepared_read_1".to_string(),
            tool_names::READ_FILE.to_string(),
            serde_json::to_string(&json!({
                "path": backing.sample_file.to_string_lossy()
            }))
            .expect("serialize tool args"),
        )),
        PreparedAssistantToolCall::new(uni::ToolCall::function(
            "prepared_read_2".to_string(),
            tool_names::READ_FILE.to_string(),
            serde_json::to_string(&json!({
                "path": second_file.to_string_lossy()
            }))
            .expect("serialize tool args"),
        )),
    ];

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut tp_ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome = handle_tool_calls(&mut outcome_ctx, &tool_calls)
        .await
        .expect("prepared tool calls should execute");

    assert!(outcome.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 2);
    assert!(!outcome_ctx.ctx.harness_state.tool_budget_exhausted());
    assert!(
        !outcome_ctx
            .ctx
            .working_history
            .iter()
            .any(|message| { message.content.as_text().contains("exceeded max tool calls per turn") })
    );
}

#[tokio::test]
async fn end_to_end_blocked_calls_do_not_burn_budget_before_valid_call() {
    let mut backing = TestContextBacking::new(1).await;
    backing.select_build_primary_agent();
    let valid_file = backing.sample_file.clone();
    let valid_args = json!({"path": valid_file.to_string_lossy()});
    cache_tool_permission(&mut backing, tool_names::READ_FILE, &valid_args, PermissionGrant::Permanent).await;

    let mut turn_modified_files: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut tp_ctx = backing.turn_processing_context();

    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut tp_ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let blocked_args = json!({"path":"/var/db/shadow"});
    let first = handle_single_tool_call(&mut outcome_ctx, "blocked_1", tool_names::READ_FILE, blocked_args.clone())
        .await
        .expect("first blocked call should not fail hard");
    assert!(first.is_none());

    let second = handle_single_tool_call(&mut outcome_ctx, "blocked_2", tool_names::READ_FILE, blocked_args)
        .await
        .expect("second blocked call should not fail hard");
    assert!(second.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 0);
    assert!(!outcome_ctx.ctx.harness_state.tool_budget_exhausted());

    let third = handle_single_tool_call(&mut outcome_ctx, "valid_1", tool_names::READ_FILE, valid_args.clone())
        .await
        .expect("valid call should execute");
    assert!(third.is_none());
    assert_eq!(outcome_ctx.ctx.harness_state.tool_calls, 1);
    assert!(outcome_ctx.ctx.harness_state.tool_budget_exhausted());

    let exhausted = handle_single_tool_call(&mut outcome_ctx, "exhausted", tool_names::READ_FILE, valid_args)
        .await
        .expect("exhausted-budget call should return structured outcome");
    // Tool-call budget exhaustion must NOT break the turn as `Blocked` — that
    // skips the synthesis pass. It rejects the call with a policy-violation
    // tool response and lets the post-batch flush push a single synthesis
    // directive (see `flush_budget_synthesis_directives`). A single rejected
    // call is below the blocked-streak cap, so the guard returns no outcome.
    assert!(exhausted.is_none());
    assert!(outcome_ctx.ctx.working_history.iter().any(|message| {
        message.role == uni::MessageRole::Tool && message.content.as_text().contains("exceeded max tool calls per turn")
    }));
}

#[tokio::test]
async fn pending_verification_blocks_patch_before_filesystem_mutation() {
    let mut backing = TestContextBacking::new(4).await;
    backing.select_build_primary_agent();
    let patch_args = json!({
        "input": "*** Begin Patch\n*** Update File: sample.txt\n@@\n-hello\n+goodbye\n*** End Patch\n"
    });
    cache_tool_permission(&mut backing, tool_names::APPLY_PATCH, &patch_args, PermissionGrant::Permanent).await;

    let mut repeated_tool_attempts = LoopTracker::new();
    repeated_tool_attempts.verification_pending = true;
    let mut turn_modified_files = BTreeSet::new();
    let sample_file = backing.sample_file.clone();
    let mut ctx = backing.turn_processing_context();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome = handle_single_tool_call(
        &mut outcome_ctx,
        "patch_while_verification_pending",
        tool_names::APPLY_PATCH,
        patch_args,
    )
    .await
    .expect("pending verification should return a structured tool response");

    assert!(outcome.is_none());
    assert_eq!(std::fs::read_to_string(sample_file).expect("read sample file"), "hello\n");
    assert!(outcome_ctx.ctx.working_history.iter().any(|message| {
        message.role == uni::MessageRole::Tool
            && message.content.as_text().contains("anti_blind_editing_verification_required")
    }));
    let response = outcome_ctx
        .ctx
        .working_history
        .iter()
        .rev()
        .find(|message| message.role == uni::MessageRole::Tool)
        .expect("blocked tool response");
    let content = response.content.as_text();
    assert!(content.contains("earlier turn"));
    assert!(content.contains("\"pending_mutations\":null"));
    assert!(content.contains("\"pending_mutation_count_known\":false"));
    assert!(!content.contains("0 effective file changes"));
}

#[tokio::test]
async fn repeated_read_only_guard_dedups_plan_file_in_planning_mode() {
    let mut backing = TestContextBacking::new(4).await;
    backing.select_build_primary_agent();

    // Create a plan file inside the temporary workspace.
    let workspace = backing.sample_file.parent().unwrap().to_path_buf();
    let plans_dir = workspace.join("plans");
    std::fs::create_dir_all(&plans_dir).expect("create plans dir");
    let plan_path = plans_dir.join("modular-dreaming-pixel.md");
    let plan_content = "# Plan\n\n1. Fix planning-mode clarity\n2. Throttle memory envelopes\n";
    std::fs::write(&plan_path, plan_content).expect("write plan file");

    let mut ctx = backing.turn_processing_context();
    ctx.tool_registry.enable_planning();

    let args = json!({
        "path": plan_path.to_string_lossy()
    });

    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let first = handle_single_tool_call(&mut outcome_ctx, "read_plan_1", tool_names::READ_FILE, args.clone())
        .await
        .expect("first plan-file read should succeed");
    assert!(first.is_none(), "first read should execute normally");

    let second = handle_single_tool_call(&mut outcome_ctx, "read_plan_2", tool_names::READ_FILE, args)
        .await
        .expect("second plan-file read should be deduplicated");
    assert!(second.is_none(), "duplicate read should be handled by guard");

    assert!(outcome_ctx.ctx.working_history.iter().any(|message| {
        message.role == uni::MessageRole::Tool && message.content.as_text().contains("\"reused_recent_result\":true")
    }));
}

#[tokio::test]
async fn planning_mode_allows_request_user_input_blocked_through_to_failure() {
    let mut backing = TestContextBacking::new(4).await;
    backing.tool_registry.enable_planning();
    let mut ctx = backing.turn_processing_context();
    let args = json!({"questions": [{"id": "q1", "header": "Q1", "question": "Test?"}]});

    let outcome = enforce_blocked_tool_call_guard(&mut ctx, "interview_blocked", tool_names::REQUEST_USER_INPUT, &args);

    assert!(
        outcome.is_none(),
        "Blocked request_user_input should flow through to handle_failure, not fast-exit from guard"
    );
    assert!(
        !ctx.plan_session.is_interview_denied(),
        "Interview denial is handled by handle_failure in execution_result.rs, not the guard"
    );
    assert_eq!(ctx.harness_state.consecutive_blocked_tool_calls, 1, "blocked call should be recorded");
}

#[tokio::test]
async fn permanent_interview_denial_switches_to_tool_free_plan_recovery() {
    let mut backing = TestContextBacking::new(4).await;
    backing.tool_registry.enable_planning();
    backing
        .tool_registry
        .set_tool_policy(tool_names::REQUEST_USER_INPUT, ToolPolicy::Deny)
        .await
        .expect("deny interview tool");

    let mut ctx = backing.turn_processing_context();
    let call = PreparedAssistantToolCall::new(uni::ToolCall::function(
        "interview_denied".to_string(),
        tool_names::REQUEST_USER_INPUT.to_string(),
        json!({"questions": [{"id": "q1", "header": "Q1", "question": "Test?"}]}).to_string(),
    ));
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome = handle_tool_calls(&mut outcome_ctx, &[call])
        .await
        .expect("denied interview should be handled as a tool response");

    assert!(outcome.is_none());
    assert!(outcome_ctx.ctx.plan_session.is_interview_denied());
    assert!(outcome_ctx.ctx.harness_state.is_recovery_active());
    assert!(outcome_ctx.ctx.harness_state.recovery_is_tool_free());
    assert!(outcome_ctx.ctx.working_history.iter().any(|message| {
        message
            .content
            .as_text()
            .contains("synthesize exactly one completed `<proposed_plan>`")
    }));
}

#[tokio::test]
async fn planning_mode_non_planning_tool_blocked_calls_schedule_recovery() {
    let mut backing = TestContextBacking::new(4).await;
    backing.tool_registry.enable_planning();
    let mut ctx = backing.turn_processing_context();
    let args = json!({"path": "src/main.rs"});
    let max_streak = max_consecutive_blocked_tool_calls_per_turn(&ctx);

    for idx in 0..max_streak {
        let outcome =
            enforce_blocked_tool_call_guard(&mut ctx, &format!("blocked_{idx}"), tool_names::READ_FILE, &args);
        assert!(outcome.is_none(), "non-planning blocked call {idx} should stay under cap");
    }

    let outcome = enforce_blocked_tool_call_guard(&mut ctx, "blocked_over_cap", tool_names::READ_FILE, &args);
    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
    assert!(ctx.harness_state.blocked_tool_recovery_pending());
}

#[tokio::test]
async fn blocked_batch_drains_responses_and_schedules_tool_free_recovery() {
    let mut backing = TestContextBacking::new(4).await;
    backing
        .tool_registry
        .set_tool_policy(tool_names::READ_FILE, ToolPolicy::Deny)
        .await
        .expect("deny read_file");
    let mut ctx = backing.turn_processing_context();
    ctx.full_auto = true;
    let args_for_call = |index: usize| json!({"path": format!("src/main_{index}.rs")});
    let max_streak = max_consecutive_blocked_tool_calls_per_turn(&ctx);
    let tool_calls = (0..=max_streak)
        .map(|index| {
            PreparedAssistantToolCall::new(uni::ToolCall::function(
                format!("blocked_batch_{index}"),
                tool_names::READ_FILE.to_string(),
                args_for_call(index).to_string(),
            ))
        })
        .collect::<Vec<_>>();
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };

    let outcome = handle_tool_calls(&mut outcome_ctx, &tool_calls)
        .await
        .expect("blocked batch should schedule recovery");

    assert!(matches!(outcome, Some(TurnHandlerOutcome::Continue)));
    assert!(outcome_ctx.ctx.harness_state.is_recovery_active());
    assert!(outcome_ctx.ctx.harness_state.recovery_is_tool_free());
    assert_eq!(
        outcome_ctx
            .ctx
            .working_history
            .iter()
            .filter(|message| message.role == uni::MessageRole::Tool)
            .count(),
        tool_calls.len(),
        "every assistant tool call needs a response before recovery"
    );
    assert!(outcome_ctx.ctx.working_history.iter().any(|message| {
        message.role == uni::MessageRole::System
            && message.content.as_text().contains("tools are disabled for one bounded pass")
    }));
}

#[tokio::test]
async fn single_tool_call_dispatch_records_requested_tool_calls() {
    // Checkpoints turn_912/913/917 showed `requested_tool_calls: 0` alongside
    // `admitted_tool_calls: N`: only the full-auto batch path recorded the
    // requested count. Recording moved to the shared dispatch so every path
    // counts identically.
    let mut backing = TestContextBacking::new(4).await;
    let valid_file = backing.sample_file.clone();
    let valid_args = json!({"path": valid_file.to_string_lossy()});
    cache_tool_permission(&mut backing, tool_names::READ_FILE, &valid_args, PermissionGrant::Permanent).await;

    let mut ctx = backing.turn_processing_context();
    let mut repeated_tool_attempts = LoopTracker::new();
    let mut turn_modified_files = BTreeSet::new();
    let mut outcome_ctx = ToolOutcomeContext {
        ctx: &mut ctx,
        repeated_tool_attempts: &mut repeated_tool_attempts,
        turn_modified_files: &mut turn_modified_files,
    };
    let call = PreparedAssistantToolCall::new(uni::ToolCall::function(
        "single_call".to_string(),
        tool_names::READ_FILE.to_string(),
        valid_args.to_string(),
    ));
    handle_tool_calls(&mut outcome_ctx, &[call])
        .await
        .expect("single valid call should execute");

    let diagnostics = ctx.harness_state.snapshot_turn_diagnostics(Default::default(), 0);
    assert_eq!(diagnostics.requested_tool_calls, 1, "single-call dispatch must record requested tool calls");
    assert_eq!(diagnostics.admitted_tool_calls, 1);
}
