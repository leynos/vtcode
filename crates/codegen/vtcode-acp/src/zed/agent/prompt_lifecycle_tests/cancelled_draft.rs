//! Deterministic coverage for committing a cancelled turn's visible draft.
//!
//! The parent module supplies the scripted provider and duplex harness; these
//! cases pin what happens to assistant text the client saw before cancellation.

use super::*;

use crate::zed::agent::ZedAgent;
use crate::zed::agent::handlers::commit_cancelled_draft_for_test;

/// Assistant text recorded in the session's durable history.
fn assistant_history(agent: &ZedAgent, session_id: &crate::acp::SessionId) -> Vec<String> {
    let session = agent
        .session_handle(session_id)
        .expect("look up ACP session")
        .expect("ACP session exists");
    agent
        .resolved_messages(&session)
        .expect("resolve ACP session messages")
        .into_iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.content.as_text().into_owned())
        .collect()
}

/// A partial draft streamed before a provider failure, with cancellation
/// already observed when the failure is classified, must still end with
/// `Cancelled`. The draft reached the client once, so the finalizer must not
/// resend it, and durable history must record it because the client saw it.
#[tokio::test]
async fn cancel_after_partial_stream_failure_persists_visible_draft() {
    let harness = streaming_lifecycle_harness(vec![ScriptedStep::PartialThenFailure("partial draft")], false).await;
    let agent = std::sync::Arc::clone(&harness.agent);
    agent.checkpoint_pause.arm("provider_failure_classified");

    let notifications = run_client(&harness, async move |cx, workspace, _provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let mut prompt = Box::pin(cx.send_request(prompt_request(&session_id, "partial prompt")).block_task());
        let _first_poll = poll_once(prompt.as_mut()).await;
        // The stream emitted its token and failed; the turn is paused before
        // deciding whether cancellation outranks the failure.
        agent.checkpoint_pause.reached().await;
        cx.send_notification(CancelNotification::new(session_id.clone()))?;
        wait_for_cancellation(&agent, &session_id).await;
        agent.checkpoint_pause.release();

        let response = prompt.await?;
        assert_eq!(response.stop_reason, StopReason::Cancelled);
        assert_eq!(
            assistant_history(&agent, &session_id),
            vec!["partial draft".to_string()],
            "the visible draft must be recorded in durable history exactly once"
        );
        Ok(())
    })
    .await;

    assert_eq!(
        assistant_messages(&notifications),
        vec!["partial draft".to_string()],
        "the streamed draft must reach the client exactly once, not again from the finalizer"
    );
}

/// The buffered path reaches the cancellation finalizer with no draft at all:
/// cancellation observed before a provider failure is classified must still
/// answer `Cancelled` and emit nothing.
#[tokio::test]
async fn cancel_before_buffered_failure_finalization_returns_cancelled() {
    let harness = lifecycle_harness(vec![ScriptedStep::Failure], true).await;
    let agent = std::sync::Arc::clone(&harness.agent);
    agent.checkpoint_pause.arm("provider_failure_classified");

    let notifications = run_client(&harness, async move |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let prompt = hold_first_prompt_until_provider_starts(
            &provider,
            Box::pin(cx.send_request(prompt_request(&session_id, "fail prompt")).block_task()),
        )
        .await;
        provider.release_call();
        // The provider failure is now classified; cancel while the turn is
        // paused before the decision, then release with cancellation observed.
        agent.checkpoint_pause.reached().await;
        cx.send_notification(CancelNotification::new(session_id.clone()))?;
        wait_for_cancellation(&agent, &session_id).await;
        agent.checkpoint_pause.release();

        let response = prompt.await?;
        assert_eq!(response.stop_reason, StopReason::Cancelled);
        assert!(
            assistant_history(&agent, &session_id).is_empty(),
            "a cancelled buffered failure with no draft must not record assistant history"
        );
        Ok(())
    })
    .await;

    assert!(
        assistant_messages(&notifications).is_empty(),
        "a cancelled buffered failure must not send a draft or a failure notice"
    );
}

/// Commitment seam: a flushed draft is published to the client once and
/// appended to durable history.
#[tokio::test]
async fn cancelled_draft_reaches_the_client_once_and_is_persisted() {
    let harness = lifecycle_harness(Vec::new(), false).await;
    let agent = std::sync::Arc::clone(&harness.agent);

    let notifications = run_client(&harness, async move |cx, workspace, _provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let session = agent
            .session_handle(&session_id)
            .expect("look up ACP session")
            .expect("ACP session exists");
        commit_cancelled_draft_for_test(&agent, &session, &session_id, "partial draft", false).await;
        assert_eq!(
            assistant_history(&agent, &session_id),
            vec!["partial draft".to_string()],
            "the flushed draft must be appended to durable history"
        );
        Ok(())
    })
    .await;

    assert_eq!(
        assistant_messages(&notifications),
        vec!["partial draft".to_string()],
        "a flushed draft must be published exactly once"
    );
}

/// Commitment seam: text the client already saw is not published again, but it
/// is still appended to durable history so both agree.
#[tokio::test]
async fn already_streamed_cancelled_draft_is_only_persisted() {
    let harness = lifecycle_harness(Vec::new(), false).await;
    let agent = std::sync::Arc::clone(&harness.agent);

    let notifications = run_client(&harness, async move |cx, workspace, _provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let session = agent
            .session_handle(&session_id)
            .expect("look up ACP session")
            .expect("ACP session exists");
        commit_cancelled_draft_for_test(&agent, &session, &session_id, "streamed draft", true).await;
        assert_eq!(
            assistant_history(&agent, &session_id),
            vec!["streamed draft".to_string()],
            "already-visible text must still be appended to durable history"
        );
        Ok(())
    })
    .await;

    assert!(
        assistant_messages(&notifications).is_empty(),
        "text the client already streamed must not be published a second time"
    );
}

/// Commitment seam: publication has no client here, so it fails; durable
/// history must still be written and the commit must not propagate the error.
#[tokio::test]
async fn cancelled_draft_persists_when_publication_fails() {
    let harness = lifecycle_harness(Vec::new(), false).await;
    let session_id = harness.agent.register_session().expect("register an ACP session");
    let session = harness
        .agent
        .session_handle(&session_id)
        .expect("look up ACP session")
        .expect("ACP session exists");

    commit_cancelled_draft_for_test(&harness.agent, &session, &session_id, "draft", false).await;

    assert_eq!(
        assistant_history(&harness.agent, &session_id),
        vec!["draft".to_string()],
        "a publication failure must not drop a draft from durable history"
    );
}

/// Commitment seam: a poisoned session lock makes persistence fail; the commit
/// must stay best-effort and return normally instead of panicking or changing
/// the cancellation outcome.
#[tokio::test]
async fn cancelled_draft_persistence_failure_is_best_effort() {
    let harness = lifecycle_harness(Vec::new(), false).await;
    let session_id = harness.agent.register_session().expect("register an ACP session");
    let session = harness
        .agent
        .session_handle(&session_id)
        .expect("look up ACP session")
        .expect("ACP session exists");
    let data = std::sync::Arc::clone(&session.data);
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = data.lock().expect("lock session data to poison it");
        panic!("poison the session data lock for the cancellation seam");
    }));
    assert!(poisoned.is_err(), "the session data lock must be poisoned for this case");

    commit_cancelled_draft_for_test(&harness.agent, &session, &session_id, "draft", false).await;
}
