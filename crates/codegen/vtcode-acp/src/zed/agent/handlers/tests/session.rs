//! Checks ACP session capabilities, turn exclusion, recovery, and lifecycle-hook effects.
use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use std::time::Duration;

#[test]
fn advertised_capabilities_include_session_discovery_and_resume() {
    let capabilities = advertised_agent_capabilities(false, false);

    assert!(capabilities.load_session);
    assert!(capabilities.session_capabilities.list.is_some());
    assert!(capabilities.session_capabilities.resume.is_some());
    assert!(capabilities.session_capabilities.close.is_some());
}

#[tokio::test]
async fn close_session_cancels_and_waits_for_admitted_prompt_work() {
    let workspace = TempDir::new().expect("temporary ACP workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session_id = agent.register_session().expect("register ACP session");
    let session = agent
        .session_handle(&session_id)
        .expect("look up ACP session")
        .expect("registered ACP session");
    let prompt_guard = agent.begin_session_prompt(&session_id).expect("admit prompt work");

    let closing_agent = Arc::clone(&agent);
    let closing_session_id = session_id.clone();
    let close_task = tokio::spawn(async move { closing_agent.close_session(&closing_session_id).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        while !session.cancellation.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session close should publish cancellation");
    assert!(!close_task.is_finished(), "close must wait for admitted prompt work");
    assert!(agent.session_handle(&session_id).expect("session lookup after close").is_none());
    assert!(agent.begin_session_prompt(&session_id).is_err(), "close must reject new prompt admission");

    drop(prompt_guard);
    let response = tokio::time::timeout(Duration::from_secs(1), close_task)
        .await
        .expect("close should finish after prompt work is released")
        .expect("close task should complete")
        .expect("session close should succeed");
    drop(response);
    assert!(session.cancellation.is_closed());
    assert!(
        session
            .mark_session_ended()
            .expect("session end state should be readable")
            .is_none(),
        "session close should run the SessionEnd boundary before succeeding"
    );
}

#[tokio::test]
async fn close_wait_is_bounded_and_keeps_the_runtime_until_prompt_work_quiesces() {
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let session_workspace = TempDir::new().expect("temporary session workspace");
    let agent = Arc::new(build_wire_test_agent(launch_workspace.path()).await);
    let session_id = agent
        .new_session(NewSessionRequest::new(session_workspace.path()))
        .await
        .expect("create ACP session")
        .session_id;
    let session = agent
        .session_handle(&session_id)
        .expect("look up ACP session")
        .expect("registered ACP session");
    let runtime = session
        .workspace_runtime()
        .expect("session runtime state")
        .expect("workspace runtime");
    let prompt_guard = agent.begin_session_prompt(&session_id).expect("admit prompt work");

    let closing_agent = Arc::clone(&agent);
    let closing_session_id = session_id.clone();
    let close_task = tokio::spawn(async move { closing_agent.close_session(&closing_session_id).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !session.cancellation.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session close should publish cancellation");

    let close_result = tokio::time::timeout(Duration::from_secs(5), close_task)
        .await
        .expect("close request should remain bounded")
        .expect("close task should complete");
    assert!(close_result.is_err(), "close must not report success before prompt work is quiescent");
    assert!(!runtime.is_shutdown(), "the workspace lease must remain while the prompt is still active");

    drop(prompt_guard);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !runtime.is_shutdown() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached cleanup should shut down the last workspace runtime after prompt quiescence");
}

#[tokio::test]
async fn session_close_is_registered_on_the_wire() {
    let workspace = TempDir::new().expect("temporary ACP workspace");
    let client_workspace = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-close-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            let initialized = cx
                .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;
            assert!(initialized.agent_capabilities.session_capabilities.close.is_some());
            let session = cx.send_request(NewSessionRequest::new(client_workspace)).block_task().await?;
            let _close_response = cx
                .send_request(CloseSessionRequest::new(session.session_id))
                .block_task()
                .await?;
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("session close protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
    assert!(agent.sessions.lock().expect("ACP session map").is_empty());
}

#[test]
fn failed_turn_releases_session_and_preserves_gathered_context() {
    let thread = ThreadManager::new().start_thread_with_identifier("acp-recovery", ThreadBootstrap::new(None));
    thread.append_message(Message::user("original request".to_string()));

    {
        let _guard = TurnGuard::begin(thread.clone()).expect("first turn should begin");
        let failed_turn = IncompleteProviderTurn::from_failure(
            "context gathered from a file read",
            "partial reasoning",
            "\u{1b}[31m502 Bad Gateway\u{1b}[0m",
        );
        assert_eq!(failed_turn.response.stop_reason, acp::StopReason::EndTurn);
        assert!(failed_turn.visible_update.starts_with("\n\n"));
        assert!(failed_turn.visible_update.contains("You can retry the prompt"));
        assert!(!failed_turn.visible_update.contains('\u{1b}'));
        thread.append_message(failed_turn.message);
    }

    let messages = thread.messages();
    assert_eq!(messages.len(), 2, "failed turn history must remain available to continue");
    assert!(messages[1].content.as_text().contains("context gathered from a file read"));
    assert!(messages[1].content.as_text().contains("You can retry the prompt"));
    assert_eq!(messages[1].reasoning.as_deref(), Some("partial reasoning"));
    let metadata = messages[1].metadata.as_ref().expect("incomplete response metadata");
    assert!(metadata.is_incomplete());
    assert_eq!(metadata.incomplete_reason(), Some("502 Bad Gateway"));
    let next_turn = thread.begin_turn().expect("failed turn must release the in-flight marker");
    drop(next_turn);
    thread.finish_turn();
}

#[test]
fn concurrent_prompt_is_rejected_without_mutating_history() {
    let thread = ThreadManager::new().start_thread_with_identifier("acp-serial", ThreadBootstrap::new(None));
    let _guard = TurnGuard::begin(thread.clone()).expect("first turn should begin");

    assert!(TurnGuard::begin(thread.clone()).is_err());
    assert!(thread.messages().is_empty());
}

#[tokio::test]
async fn configured_non_stop_hooks_do_not_disable_streaming() {
    use assert_fs::TempDir;
    use vtcode_core::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
    use vtcode_core::hooks::{LifecycleHookEngine, SessionStartTrigger};

    let workspace = TempDir::new().expect("temporary hook workspace");
    let config = HooksConfig {
        lifecycle: LifecycleHooksConfig {
            user_prompt_submit: vec![HookGroupConfig {
                matcher: None,
                hooks: vec![HookCommandConfig {
                    command: "printf ok".to_string(),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        },
    };
    let engine = LifecycleHookEngine::new_with_session_gated(
        workspace.path().to_path_buf(),
        &config,
        SessionStartTrigger::NewSession,
        "acp-non-stop-streaming",
        false,
    )
    .expect("non-stop hook engine")
    .expect("configured non-stop hook");

    assert!(!engine.has_stop_hooks());
    assert!(should_allow_streaming(true, false, engine.has_stop_hooks()));
    assert!(should_allow_streaming(true, true, engine.has_stop_hooks()));
    assert!(!should_allow_streaming(false, false, engine.has_stop_hooks()));
    assert!(!should_allow_streaming(true, true, true));
}

#[test]
fn finalization_only_emits_buffered_responses() {
    assert!(!should_emit_buffered_final_chunk(true), "streamed deltas are already visible");
    assert!(should_emit_buffered_final_chunk(false), "buffered drafts need one final chunk");
}

#[tokio::test]
async fn blocked_stop_draft_is_not_visible_until_hook_allows_it() {
    use assert_fs::TempDir;
    use vtcode_core::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
    use vtcode_core::hooks::{LifecycleHookEngine, SessionStartTrigger};

    let workspace = TempDir::new().expect("temporary hook workspace");
    let config = HooksConfig {
            lifecycle: LifecycleHooksConfig {
                stop: vec![HookGroupConfig {
                    matcher: None,
                    hooks: vec![HookCommandConfig {
                        command: r#"count=$(cat "$VT_PROJECT_DIR/stop-count" 2>/dev/null || printf 0); count=$((count + 1)); printf '%s' "$count" > "$VT_PROJECT_DIR/stop-count"; if [ "$count" -eq 1 ]; then printf '%s' '{"continue":false,"stopReason":"retry the draft"}'; fi"#
                            .to_string(),
                        ..HookCommandConfig::default()
                    }],
                }],
                ..LifecycleHooksConfig::default()
            },
        };
    let engine = LifecycleHookEngine::new_with_session_gated(
        workspace.path().to_path_buf(),
        &config,
        SessionStartTrigger::NewSession,
        "acp-stop-visibility",
        false,
    )
    .expect("stop hook engine")
    .expect("stop hook should be configured");

    assert!(engine.has_stop_hooks());
    let first = engine.run_stop("blocked draft", false).await.expect("first stop hook");
    let second = engine.run_stop("allowed draft", true).await.expect("second stop hook");

    let mut visible = Vec::new();
    let mut history = Vec::new();
    if let Some(reason) = first.block_reason {
        history.push(Message::assistant("blocked draft".to_string()));
        history.push(Message::system(reason));
    } else {
        visible.push("blocked draft".to_string());
    }
    if second.block_reason.is_none() {
        visible.push("allowed draft".to_string());
    }

    assert_eq!(visible, vec!["allowed draft"]);
    assert_eq!(history.len(), 2, "blocked drafts belong in model history only");
    assert_eq!(history[0].content.as_text(), "blocked draft");
    assert_eq!(history[1].content.as_text(), "retry the draft");
}
