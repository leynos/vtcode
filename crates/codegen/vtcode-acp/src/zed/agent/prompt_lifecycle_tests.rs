//! Deterministic ACP transport tests for prompt serialization and cancellation.
//!
//! These tests drive the real SACP duplex transport and the real
//! `install_handlers` wiring against a scripted provider. They deliberately do
//! not use the process-global `OnceLock` connection handle: each test owns its
//! own `Channel::duplex()` connection so overlapping prompts, steering and
//! cancellation can be ordered precisely.
//!
//! The scripted provider, temporary-workspace agent and client runner live in
//! [`support`].

mod support;

use crate::acp::{CancelNotification, StopReason};
use std::time::Duration;
use support::*;

/// Two prompts sent while the first turn is active must both receive their own
/// response. The first turn must finish before the second starts, so no
/// assistant update may interleave.
#[tokio::test]
async fn overlapping_prompts_run_sequentially_and_both_answer() {
    let harness = lifecycle_harness(
        vec![
            ScriptedStep::Answer("first-answer"),
            ScriptedStep::Answer("second-answer"),
        ],
        true,
    )
    .await;

    let agent = std::sync::Arc::clone(&harness.agent);
    let notifications = run_client(&harness, async move |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let first = hold_first_prompt_until_provider_starts(
            &provider,
            Box::pin(cx.send_request(prompt_request(&session_id, "first prompt")).block_task()),
        )
        .await;

        // One poll writes the request frame without waiting for a response.
        let mut second = Box::pin(cx.send_request(prompt_request(&session_id, "second prompt")).block_task());
        let _first_poll = poll_once(second.as_mut()).await;
        // Wait until the steer is queued on the gate before releasing the first
        // turn. The stop reasons and provider call count then prove it ran
        // only after the first turn completed.
        wait_for_gate_waiters(&agent, &session_id, 1).await;

        provider.release_call();
        let first_response = first.await?;
        let second_response = second.await?;
        assert_eq!(first_response.stop_reason, StopReason::EndTurn);
        assert_eq!(second_response.stop_reason, StopReason::EndTurn);
        Ok(())
    })
    .await;

    assert_eq!(harness.provider.call_count(), 2, "each prompt must reach the provider once");
    assert_eq!(
        assistant_messages(&notifications),
        vec!["first-answer".to_string(), "second-answer".to_string()],
        "each turn must send its own assistant content without interleaving"
    );
}

/// A cancelled turn must not poison a prompt that was queued behind it.
#[tokio::test]
async fn cancel_then_immediate_prompt_starts_a_fresh_turn() {
    let harness = lifecycle_harness(
        vec![
            ScriptedStep::Answer("first-answer"),
            ScriptedStep::Answer("second-answer"),
        ],
        true,
    )
    .await;

    let notifications = run_client(&harness, async |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let first = hold_first_prompt_until_provider_starts(
            &provider,
            Box::pin(cx.send_request(prompt_request(&session_id, "first prompt")).block_task()),
        )
        .await;

        cx.send_notification(CancelNotification::new(session_id.clone()))?;
        // Frames are FIFO, so the cancel reaches the agent before the new
        // prompt. The cancel drops the held provider future, so the first
        // turn resolves without releasing it.
        let mut second = Box::pin(cx.send_request(prompt_request(&session_id, "second prompt")).block_task());
        // Keep the frame ordering: one poll writes the new prompt, and the
        // stop reasons below prove it ran as a fresh turn.
        let _first_poll = poll_once(second.as_mut()).await;

        let first_response = first.await?;
        let second_response = second.await?;
        assert_eq!(first_response.stop_reason, StopReason::Cancelled);
        assert_eq!(second_response.stop_reason, StopReason::EndTurn);
        Ok(())
    })
    .await;

    assert_eq!(
        assistant_messages(&notifications),
        vec!["second-answer".to_string()],
        "the cancelled turn must stay silent and the queued steer must answer"
    );
}

/// A steer queued before `session/cancel` must survive the cancellation aimed
/// at the turn that was active when it arrived.
#[tokio::test]
async fn queued_steer_survives_cancel_aimed_at_the_active_turn() {
    let harness = lifecycle_harness(
        vec![
            ScriptedStep::Answer("first-answer"),
            ScriptedStep::Answer("steer-answer"),
        ],
        true,
    )
    .await;

    let agent = std::sync::Arc::clone(&harness.agent);
    let notifications = run_client(&harness, async move |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let first = hold_first_prompt_until_provider_starts(
            &provider,
            Box::pin(cx.send_request(prompt_request(&session_id, "first prompt")).block_task()),
        )
        .await;

        // Queue the steer first, then cancel the turn it arrived during.
        let mut steer = Box::pin(cx.send_request(prompt_request(&session_id, "steer prompt")).block_task());
        let _first_poll = poll_once(steer.as_mut()).await;
        // Wait until the steer is admitted and queued on the gate: only then is
        // the cancel unambiguously aimed at the turn the steer waited for. The
        // stop reasons below prove the steer survived that cancellation.
        wait_for_gate_waiters(&agent, &session_id, 1).await;
        cx.send_notification(CancelNotification::new(session_id.clone()))?;

        let first_response = first.await?;
        let steer_response = steer.await?;
        assert_eq!(first_response.stop_reason, StopReason::Cancelled);
        assert_eq!(steer_response.stop_reason, StopReason::EndTurn);
        Ok(())
    })
    .await;

    assert_eq!(
        assistant_messages(&notifications),
        vec!["steer-answer".to_string()],
        "cancelling the active turn must not cancel or silence the queued steer"
    );
}

/// A provider that completes without content must produce an explicit error,
/// not a silent `EndTurn`.
#[tokio::test]
async fn empty_provider_output_reports_empty_response() {
    let harness = lifecycle_harness(vec![ScriptedStep::Empty], false).await;

    let notifications = run_client(&harness, async |cx, workspace, _provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let error = cx
            .send_request(prompt_request(&session_id, "empty prompt"))
            .block_task()
            .await
            .expect_err("an empty provider turn must not be acknowledged as EndTurn");
        assert_eq!(error_reason(&error), Some("empty_response"));
        Ok(())
    })
    .await;

    assert!(assistant_messages(&notifications).is_empty(), "an empty turn must not emit assistant content");
}

/// A scripted provider failure racing `session/cancel` must still answer with
/// `Cancelled`, as ACP requires. Cancellation normally wins the provider race,
/// dropping the failing future; the narrower failure-already-in-hand window is
/// covered by the buffered-text flush unit tests in `handlers/prompt`.
#[tokio::test]
async fn provider_failure_after_cancel_returns_cancelled() {
    let harness = lifecycle_harness(vec![ScriptedStep::Failure], true).await;

    let notifications = run_client(&harness, async |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let prompt = hold_first_prompt_until_provider_starts(
            &provider,
            Box::pin(cx.send_request(prompt_request(&session_id, "fail prompt")).block_task()),
        )
        .await;
        cx.send_notification(CancelNotification::new(session_id.clone()))?;
        let response = prompt.await?;
        assert_eq!(response.stop_reason, StopReason::Cancelled);
        Ok(())
    })
    .await;

    assert!(
        assistant_messages(&notifications).is_empty(),
        "a cancelled failure must not send a provider failure notice"
    );
}

/// A stream that ends before any completion event is transient, so it must be
/// retried. Once the retry budget is exhausted without assistant content, the
/// turn reports `empty_response` instead of a generic failure or a silent
/// `EndTurn`.
///
/// The clock is paused so the provider's back-off (10s then ~20s by default)
/// costs virtual time; the client timeout only has to exceed that sum.
#[tokio::test(start_paused = true)]
async fn truncated_stream_retries_then_reports_empty_response() {
    let harness = streaming_lifecycle_harness(vec![ScriptedStep::Empty], false).await;

    let notifications =
        run_client_with_timeout(&harness, Duration::from_secs(120), async |cx, workspace, _provider| {
            let session_id = initialize_and_create_session(&cx, workspace).await?;
            let error = cx
                .send_request(prompt_request(&session_id, "truncated prompt"))
                .block_task()
                .await
                .expect_err("a truncated provider stream must not be acknowledged as EndTurn");
            assert_eq!(error_reason(&error), Some("empty_response"));
            Ok(())
        })
        .await;

    assert_eq!(harness.provider.call_count(), 3, "a truncated stream must exhaust the retry budget before failing");
    assert!(assistant_messages(&notifications).is_empty(), "a truncated turn must not emit assistant content");
}
