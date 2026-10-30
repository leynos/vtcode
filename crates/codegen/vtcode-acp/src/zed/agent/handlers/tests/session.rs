//! Checks ACP session capabilities, turn serialization, recovery, and lifecycle-hook effects.
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
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];
    assert_eq!(lody["usage"]["version"], 1);
    assert_eq!(lody["compaction"]["version"], 1);
    assert_eq!(lody["rateLimits"]["version"], 1);
    assert!(lody["rateLimits"].get("query").is_none(), "header observations are push-only");
    assert!(lody.get("subagents").is_none());
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
            SafeProviderProjection::execution_error(),
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
    assert_eq!(metadata.incomplete_reason(), Some("Provider failure category: Execution failed."));
    let next_turn = thread.begin_turn().expect("failed turn must release the in-flight marker");
    drop(next_turn);
    thread.finish_turn();
}

#[test]
fn turn_guard_still_rejects_an_overlapping_turn() {
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
    assert_first_stop_hook_block(&first, workspace.path());
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

use tokio::sync::Notify;

struct BlockingPromptProvider {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMProvider for BlockingPromptProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        // Hold only the first turn open. A queued steer reaches the provider
        // after the gate is released, so it must not block here as well.
        if call_index == 0 {
            self.release.notified().await;
        }
        let response = LLMResponse::new("wire-model", format!("wire response {call_index}"));
        Ok(Box::pin(futures::stream::iter([Ok(LLMStreamEvent::Completed { response: Box::new(response) })])))
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("blocking prompt test must use streaming")
    }
}

#[tokio::test]
async fn concurrent_prompt_is_serialized_over_official_acp_transport() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_started = Arc::clone(&started);
    let provider_release = Arc::clone(&release);
    let provider_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(BlockingPromptProvider {
                started: Arc::clone(&provider_started),
                release: Arc::clone(&provider_release),
                calls: Arc::clone(&provider_calls),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let inspected_agent = Arc::clone(&agent);
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-prompt-race-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);
    let wait_for_provider = Arc::clone(&started);
    let unblock_provider = Arc::clone(&release);
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx
                .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                .block_task()
                .await?;
            let first = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("first prompt"))],
                ))
                .block_task();
            tokio::pin!(first);
            tokio::select! {
                () = wait_for_provider.notified() => {}
                response = &mut first => panic!("first prompt completed before provider blockage: {response:?}"),
            }

            // Poll once so the steer frame is written while the first turn
            // still holds the session gate; it must not resolve until that
            // turn completes.
            let mut second = Box::pin(
                cx.send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("second prompt"))],
                ))
                .block_task(),
            );
            // A single poll only writes the request frame; no response can
            // arrive in the same tick. The `EndTurn` assertions below prove
            // the steer was serialized behind the active turn.
            let _first_poll =
                std::future::poll_fn(|poll_cx| std::task::Poll::Ready(Future::poll(second.as_mut(), poll_cx))).await;

            unblock_provider.notify_one();
            let first_response = first.await?;
            let second_response = second.await?;
            assert_eq!(first_response.stop_reason, acp::StopReason::EndTurn);
            assert_eq!(
                second_response.stop_reason,
                acp::StopReason::EndTurn,
                "a steer queued during an active turn must still receive a response"
            );
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("prompt race protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(calls.load(Ordering::SeqCst), 2, "each prompt must reach the provider once");
    let sessions = inspected_agent.sessions.lock().unwrap_or_else(|error| error.into_inner());
    let session = sessions.values().next().expect("wire session should remain attached");
    let messages = session.data.lock().expect("wire session data").thread.messages();
    let user_messages = messages
        .into_iter()
        .filter(|message| message.role == vtcode_core::llm::provider::MessageRole::User)
        .count();
    assert_eq!(user_messages, 2, "both serialized prompts must persist their user message");
}

use vtcode_core::subagents::SpawnBackgroundSubprocessRequest;

#[test]
fn advertised_capabilities_include_lody_subagent_management_with_subagents() {
    let capabilities = advertised_agent_capabilities(true, false);
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];

    assert_eq!(lody["subagents"]["version"], 1);
    assert_eq!(lody["subagents"]["lifecycle"], true);
    assert_eq!(lody["subagents"]["list"], true);
    assert_eq!(lody["subagents"]["cancel"], true);
    assert_eq!(lody["subagents"]["output"], true);
    assert!(lody.get("tasks").is_none());
    assert_eq!(lody["usage"]["version"], 1);
}

#[test]
fn advertised_capabilities_include_background_tasks_only_when_enabled() {
    let capabilities = advertised_agent_capabilities(true, true);
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];

    assert_eq!(lody["tasks"]["version"], 1);
    assert_eq!(lody["tasks"]["background"], true);
}

#[tokio::test]
async fn lody_subagent_management_extensions_round_trip_over_official_acp_duplex() {
    let workspace = TempDir::new().expect("Lody management workspace");
    write_lody_background_fixture(workspace.path());
    let launch_workspace = TempDir::new().expect("separate launch workspace");
    let mut agent = build_wire_test_agent(launch_workspace.path()).await;
    let mut vt_config = VTCodeConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    agent.vt_config = Some(Box::new(vt_config.clone()));
    let setup_outcome = crate::zed::agent::optional_setup::attach_acp_subagent_controller(
        crate::zed::agent::AcpSubagentControllerSetup {
            registry: &agent.local_tool_registry,
            agent_config: &agent.config,
            custom_providers: &[],
            vt_config: Some(&vt_config),
            owner_session_id: None,
            workspace_gated: false,
        },
    )
    .await;
    assert_eq!(setup_outcome, crate::zed::agent::OptionalSetupOutcome::Attached);
    let agent = Arc::new(agent);
    assert!(agent.local_tool_registry.has_subagent_controller());
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-lody-management-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let workspace_path = workspace.path().to_path_buf();
    let inspected_agent = Arc::clone(&agent);
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            let initialized = cx
                .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;
            let lody = &initialized.agent_capabilities.meta.expect("Lody capabilities")["lody"];
            assert_eq!(lody["subagents"]["list"], true);
            assert_eq!(lody["tasks"]["background"], true);

            let session = cx
                .send_request(NewSessionRequest::new(workspace_path.clone()))
                .block_task()
                .await?;
            let owned_session = inspected_agent.session_handle(&session.session_id).unwrap().unwrap();
            let controller = inspected_agent.session_subagent_controller(&owned_session).unwrap().unwrap();
            let background = controller
                .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
                    agent_type: Some("background-demo".to_string()),
                    ..SpawnBackgroundSubprocessRequest::default()
                })
                .await
                .expect("spawn owned background task");
            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": session.session_id,
                "activeOnly": false,
            }))?;
            let response = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                    params.into(),
                )))
                .block_task()
                .await?;
            assert_eq!(response["tasks"][0]["taskId"], background.id);

            let mut output = serde_json::Value::Null;
            for _ in 0..20 {
                let params = serde_json::value::to_raw_value(&serde_json::json!({
                    "sessionId": session.session_id,
                    "taskId": background.id,
                    "tail": 1,
                }))?;
                output = cx
                    .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                        crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
                        params.into(),
                    )))
                    .block_task()
                    .await?;
                if output["output"] == "lody-output-two" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert_eq!(output["output"], "lody-output-two");

            let foreign_session = cx
                .send_request(NewSessionRequest::new(workspace_path.clone()))
                .block_task()
                .await?;
            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": foreign_session.session_id,
                "activeOnly": false,
            }))?;
            let foreign_list = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                    params.into(),
                )))
                .block_task()
                .await?;
            assert_eq!(foreign_list, serde_json::json!({ "tasks": [] }));

            let foreign_owned_session = inspected_agent.session_handle(&foreign_session.session_id).unwrap().unwrap();
            let foreign_controller = inspected_agent
                .session_subagent_controller(&foreign_owned_session)
                .unwrap()
                .unwrap();
            let first_runtime = owned_session.workspace_runtime().unwrap().unwrap();
            let foreign_runtime = foreign_owned_session.workspace_runtime().unwrap().unwrap();
            assert!(
                Arc::ptr_eq(&first_runtime, &foreign_runtime),
                "session ownership must preserve shared workspace resources"
            );
            assert!(
                !Arc::ptr_eq(&controller, &foreign_controller),
                "controllers must retain independent immutable owners"
            );
            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": foreign_session.session_id, "taskId": background.id,
            }))?;
            let foreign_cancel = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
                    params.into(),
                )))
                .block_task()
                .await
                .expect_err("a sibling must not cancel the owned task");
            assert_eq!(foreign_cancel.data, Some(serde_json::json!({ "reason": "unknown_task" })));
            assert!(
                controller
                    .background_snapshot(&background.id)
                    .await
                    .unwrap()
                    .entry
                    .status
                    .is_active()
            );

            let foreign_error = lody_output_error(&cx, &foreign_session.session_id, &background.id).await;
            let unknown_error = lody_output_error(&cx, &session.session_id, "unknown-task").await;
            assert_eq!(foreign_error, unknown_error);
            assert_eq!(foreign_error.data, Some(serde_json::json!({ "reason": "unknown_task" })));

            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": session.session_id,
                "taskId": background.id,
                "reason": "test complete",
            }))?;
            let cancelled = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
                    params.into(),
                )))
                .block_task()
                .await?;
            assert_eq!(cancelled, serde_json::json!({}));

            // Exercise the registry route itself: a detached session view must
            // never execute through the shared runtime's ownerless controller.
            let registry = foreign_runtime
                .local_tool_registry
                .with_subagent_controller(Some(Arc::clone(&foreign_controller)));
            registry.mark_tool_preapproved("agent").await;
            let spawned = registry
                .execute_tool(
                    "agent",
                    serde_json::json!({
                        "action": "spawn_subprocess", "agent_type": "background-demo",
                    }),
                )
                .await
                .expect("spawn through the session registry executor");
            let sibling = foreign_controller
                .background_status_entries()
                .await
                .into_iter()
                .next()
                .expect("sibling task");
            assert_eq!(sibling.owner_session_id.as_deref(), Some(foreign_session.session_id.0.as_ref()));
            assert_ne!(
                sibling.exec_session_id, background.exec_session_id,
                "same-workspace sessions need distinct child processes"
            );
            assert_eq!(spawned["id"], sibling.id);
            let disabled = registry.with_subagent_controller(None);
            // Preapproval is consumed once, including on the successful spawn.
            disabled.mark_tool_preapproved("agent").await;
            let denied = disabled
                .execute_tool(
                    "agent",
                    serde_json::json!({
                        "action": "spawn_subprocess", "agent_type": "background-demo",
                    }),
                )
                .await
                .expect("registry execution errors are returned in the public JSON envelope");
            assert_eq!(denied["error"]["message"], "Subagent controller is not active in this session");
            assert_eq!(denied["error"]["category"], "ExecutionError");
            assert!(denied.get("id").is_none(), "an empty controller slot must not spawn a child: {denied}");
            assert!(registry.has_subagent_controller(), "detaching a sibling view must not clear its controller");

            drop(
                cx.send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            assert!(
                foreign_controller
                    .background_snapshot(&sibling.id)
                    .await
                    .unwrap()
                    .entry
                    .status
                    .is_active(),
                "closing the first session must not stop a sibling's live child process"
            );
            assert_eq!(first_runtime.lifecycle.lock().unwrap().active_sessions, 1);
            assert_eq!(
                first_runtime.lifecycle.lock().unwrap().state,
                crate::zed::agent::session_workspace::WorkspaceRuntimeLifecycleState::Open
            );
            let cancelled_sibling = foreign_controller.force_cancel_background(&sibling.id).await.unwrap();
            assert!(!cancelled_sibling.status.is_active(), "the sibling process must be stopped during test cleanup");
            drop(
                cx.send_request(CloseSessionRequest::new(foreign_session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            assert_eq!(first_runtime.lifecycle.lock().unwrap().active_sessions, 0);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("Lody management client should finish")
        .expect("Lody management protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}

async fn lody_output_error(connection: &ConnectionTo<Agent>, session_id: &acp::SessionId, task_id: &str) -> acp::Error {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
    }))
    .expect("serialize Lody output request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
            params.into(),
        )))
        .block_task()
        .await
        .expect_err("Lody output request should fail")
}

fn write_lody_background_fixture(workspace: &std::path::Path) {
    let agent_dir = workspace.join(".vtcode/agents");
    std::fs::create_dir_all(&agent_dir).expect("create Lody test agent directory");
    std::fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Lody management protocol fixture.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background fixture.
"#,
    )
    .expect("write Lody test agent");
    let scripts_dir = workspace.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create Lody test scripts directory");
    let script = scripts_dir.join("demo-background-subagent.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'lody-output-one\\nlody-output-two\\n'\nsleep 30\n")
        .expect("write Lody background script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script)
            .expect("Lody background script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("make Lody background script executable");
    }
}

fn assert_first_stop_hook_block(outcome: &vtcode_core::hooks::StopHookOutcome, project_dir: &std::path::Path) {
    let messages = outcome
        .messages
        .iter()
        .map(|message| format!("{:?}: {}", message.level, message.text.chars().take(256).collect::<String>()))
        .collect::<Vec<_>>();
    let stop_count = std::fs::read_to_string(project_dir.join("stop-count"))
        .ok()
        .map(|count| count.trim().chars().take(32).collect::<String>());

    assert_eq!(
        outcome.block_reason.as_deref(),
        Some("retry the draft"),
        "first stop hook outcome before projection: messages={messages:?}, stop-count={stop_count:?}",
    );
}

use std::path::PathBuf;
use vtcode_core::subagents::SpawnAgentRequest;

#[test]
fn acp_subagent_child_uses_single_orchestration_without_losing_tool_permissions() {
    use vtcode_config::core::agent::HarnessOrchestrationMode;

    let mut parent = VTCodeConfig::default();
    parent.agent.harness.orchestration_mode = HarnessOrchestrationMode::PlanBuildEvaluate;
    parent.permissions.allow = vec!["read".to_string()];
    let spec = vtcode_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("built-in explorer subagent");

    let child = vtcode_core::subagents::build_child_config(&parent, &spec, "gpt-5-mini", Some(1), false);

    assert_eq!(parent.agent.harness.orchestration_mode, HarnessOrchestrationMode::PlanBuildEvaluate);
    assert_eq!(child.agent.harness.orchestration_mode, HarnessOrchestrationMode::Single);
    assert_eq!(child.permissions.allow, parent.permissions.allow);
}

#[test]
fn captured_planner_fixture_is_valid_and_failure_mutation_is_empty() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock");
    let success: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixtures.join("planner-response-sanitized.json")).expect("read valid planner fixture"),
    )
    .expect("parse valid planner fixture envelope");
    let success_content = success["choices"][0]["message"]["content"]
        .as_str()
        .expect("valid planner content");
    let planner: serde_json::Value = serde_json::from_str(success_content).expect("parse captured planner JSON");
    for key in [
        "task_title",
        "spec_markdown",
        "contract_markdown",
        "feature_list_markdown",
        "items",
    ] {
        assert!(!planner[key].is_null(), "valid captured planner fixture must include {key}");
    }

    let empty: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixtures.join("planner-response-empty.json")).expect("read empty planner mutation"),
    )
    .expect("parse empty planner mutation envelope");
    assert_eq!(empty["choices"][0]["message"]["content"], "");
}

#[tokio::test]
async fn failed_child_is_distinct_from_successful_polling_and_keeps_a_durable_archive() {
    use vtcode_config::core::CustomProviderRequestPolicyConfig;
    use vtcode_config::core::agent::HarnessOrchestrationMode;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _provider_guard = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": { "message": "synthetic child provider failure" }
        })))
        .expect(1)
        .mount(&provider)
        .await;

    let workspace = TempDir::new().expect("failed child workspace");
    let child_provider = CustomProviderConfig {
        name: "wire-test".to_string(),
        display_name: "Synthetic child provider".to_string(),
        base_url: provider.uri(),
        model: "wire-model".to_string(),
        models: vec!["wire-model".to_string()],
        supports_tools: Some(true),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 0,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    let mut vt_config = VTCodeConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.agent.provider = "wire-test".to_string();
    vt_config.agent.default_model = "wire-model".to_string();
    vt_config.agent.harness.orchestration_mode = HarnessOrchestrationMode::PlanBuildEvaluate;
    vt_config.permissions.allow = vec!["read".to_string()];
    vt_config.custom_providers = vec![child_provider.clone()];
    vtcode_core::llm::factory::register_custom_providers(&vt_config.custom_providers);
    let agent = Arc::new(
        build_wire_test_agent_with_providers_and_options(
            workspace.path(),
            std::slice::from_ref(&child_provider),
            Some(&vt_config),
            None,
        )
        .await,
    );

    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-child-recovery-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);
    let workspace_path = workspace.path().to_path_buf();
    let inspected_agent = Arc::clone(&agent);
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            let owned_session = inspected_agent
                .session_handle(&session.session_id)
                .expect("failed child session lookup")
                .expect("failed child session");
            let inspected_controller = inspected_agent
                .session_subagent_controller(&owned_session)
                .expect("failed child controller lookup")
                .expect("failed child controller");
            let available_specs = inspected_controller.effective_specs().await;
            assert!(
                available_specs
                    .iter()
                    .any(|spec| spec.name == "explorer" && spec.is_subagent() && spec.is_read_only()),
                "the ACP test controller must discover the built-in read-only explorer"
            );
            let delegation_hints = inspected_controller
                .set_turn_delegation_hints_from_input(
                    "@agent-explorer inspect README.md and report whether a Usage heading exists.",
                )
                .await;
            assert_eq!(delegation_hints, ["explorer"]);
            let child = inspected_controller
                .spawn(SpawnAgentRequest {
                    agent_type: Some("explorer".to_string()),
                    message: Some(
                        "Read README.md once and report whether it contains a Usage heading; do not modify files."
                            .to_string(),
                    ),
                    max_turns: Some(1),
                    ..SpawnAgentRequest::default()
                })
                .await
                .expect("spawn synthetic child");

            let mut failed = serde_json::Value::Null;
            for _ in 0..100 {
                let params = serde_json::value::to_raw_value(&serde_json::json!({
                    "sessionId": session.session_id,
                    "activeOnly": false,
                }))?;
                let polled = cx
                    .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                        crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                        params.into(),
                    )))
                    .block_task()
                    .await?;
                if polled["tasks"][0]["status"] == "failed" {
                    failed = polled;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert_eq!(failed["tasks"][0]["taskId"], child.id);
            assert_eq!(failed["tasks"][0]["status"], "failed");
            assert!(failed["tasks"][0]["stopReason"].as_str().is_some_and(|value| !value.is_empty()));

            let active_params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": session.session_id,
                "activeOnly": true,
            }))?;
            let active = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                    active_params.into(),
                )))
                .block_task()
                .await?;
            assert_eq!(active, serde_json::json!({ "tasks": [] }));

            let snapshot = inspected_controller
                .snapshot_for_thread(&child.id)
                .await
                .expect("failed child remains inspectable");
            assert_eq!(snapshot.effective_config.agent.harness.orchestration_mode, HarnessOrchestrationMode::Single);
            assert_eq!(snapshot.effective_config.permissions.allow, ["read"]);
            let archive_path = snapshot
                .archive_path
                .unwrap_or_else(|| panic!("failed child archive path; ACP terminal status: {failed}"));
            let live_status = inspected_controller
                .status_for(&child.id)
                .await
                .expect("live failed child status");
            assert_eq!(live_status.status, vtcode_core::subagents::SubagentStatus::Failed);
            let live_error = live_status.error.as_deref().expect("failed child error");
            assert!(
                live_error.contains("synthetic child provider failure"),
                "child must reach the intended provider failure: {live_error}"
            );
            assert_eq!(failed["tasks"][0]["stopReason"].as_str(), Some(live_error));
            assert_eq!(live_status.transcript_path.as_ref(), Some(&archive_path));

            let reloaded = vtcode_core::utils::session_archive::find_session_by_identifier(&snapshot.session_id)
                .await
                .expect("reload failed child archive")
                .expect("failed child archive remains discoverable");
            assert_eq!(reloaded.path, archive_path);
            assert!(
                reloaded
                    .snapshot
                    .messages
                    .iter()
                    .any(|message| message.content.as_text().contains("Read README.md")),
                "durable failed-child history must retain the delegated task"
            );
            std::fs::remove_file(archive_path).expect("remove failed child archive");
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(8), client_connection)
        .await
        .expect("failed child ACP client should finish")
        .expect("failed child ACP protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates child request physics"]
async fn vidaimock_child_completes_without_an_optional_planner_phase() {
    use std::net::{TcpListener, TcpStream};
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::Duration as StdDuration;
    use vtcode_config::core::CustomProviderRequestPolicyConfig;
    use vtcode_config::core::agent::HarnessOrchestrationMode;

    let _provider_guard = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    struct RunningChildVidaiMock {
        child: Child,
        base_url: String,
    }

    impl Drop for RunningChildVidaiMock {
        fn drop(&mut self) {
            drop(self.child.kill());
            drop(self.child.wait());
        }
    }

    let version = Command::new("vidaimock")
        .arg("--version")
        .output()
        .expect("vidaimock --version");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("0.3.1"));
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve VidaiMock port");
    let port = listener.local_addr().expect("VidaiMock address").port();
    drop(listener);
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock");
    let mut child = Command::new("vidaimock")
        .env("VIDAIMOCK_ISOLATED", "true")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--config")
        .arg(fixtures.join("scenarios/trickled-stream.toml"))
        .arg("--config-dir")
        .arg(&fixtures)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start VidaiMock child fixture");
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(child.try_wait().expect("poll VidaiMock").is_none(), "VidaiMock exited before startup");
        thread::sleep(StdDuration::from_millis(25));
    }
    let vidaimock = RunningChildVidaiMock {
        child,
        base_url: format!("http://127.0.0.1:{port}/v1/child-single"),
    };

    let workspace = TempDir::new().expect("VidaiMock child workspace");
    std::fs::write(workspace.path().join("README.md"), "# Fixture\n\n## Usage\n").expect("write child fixture");
    let provider = CustomProviderConfig {
        name: "wire-test".to_string(),
        display_name: "VidaiMock child provider".to_string(),
        base_url: vidaimock.base_url.clone(),
        model: "wire-model".to_string(),
        models: vec!["wire-model".to_string()],
        supports_tools: Some(true),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 0,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    let mut config = VTCodeConfig::default();
    config.subagents.enabled = true;
    config.agent.provider = "wire-test".to_string();
    config.agent.default_model = "wire-model".to_string();
    config.agent.harness.orchestration_mode = HarnessOrchestrationMode::PlanBuildEvaluate;
    config.permissions.allow = vec!["read".to_string()];
    config.custom_providers = vec![provider.clone()];
    vtcode_core::llm::factory::register_custom_providers(&config.custom_providers);
    let agent = Arc::new(
        build_wire_test_agent_with_providers_and_options(
            workspace.path(),
            std::slice::from_ref(&provider),
            Some(&config),
            None,
        )
        .await,
    );
    let session = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("VidaiMock child session");
    let owned_session = agent
        .session_handle(&session.session_id)
        .expect("VidaiMock session lookup")
        .expect("VidaiMock owned session");
    let controller = agent
        .session_subagent_controller(&owned_session)
        .expect("VidaiMock controller lookup")
        .expect("VidaiMock child controller");
    let available_specs = controller.effective_specs().await;
    assert!(
        available_specs
            .iter()
            .any(|spec| spec.name == "explorer" && spec.is_subagent() && spec.is_read_only()),
        "the VidaiMock controller must discover the built-in read-only explorer"
    );
    let hints = controller
        .set_turn_delegation_hints_from_input("@agent-explorer inspect README.md for a Usage heading")
        .await;
    assert_eq!(hints, ["explorer"]);
    let spawned = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("explorer".to_string()),
            message: Some("Read README.md and report whether a Usage heading exists; do not modify files.".to_string()),
            max_turns: Some(1),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn VidaiMock child");
    let terminal = controller
        .wait(std::slice::from_ref(&spawned.id), Some(8_000))
        .await
        .expect("wait for VidaiMock child")
        .expect("VidaiMock child reached a terminal state");
    assert_eq!(
        terminal.status,
        vtcode_core::subagents::SubagentStatus::Completed,
        "child failure: {:?}",
        terminal.error
    );
    assert!(terminal.summary.as_deref().is_some_and(|summary| !summary.trim().is_empty()));
    let snapshot = controller
        .snapshot_for_thread(&spawned.id)
        .await
        .expect("VidaiMock child snapshot");
    assert!(
        snapshot.snapshot.messages.iter().any(|message| {
            message.role == vtcode_core::llm::provider::MessageRole::Assistant
                && message.content.as_text().contains("Synthetic child completed")
        }),
        "child history must preserve the actual simulated response"
    );
    assert_eq!(snapshot.effective_config.agent.harness.orchestration_mode, HarnessOrchestrationMode::Single);
    assert_eq!(snapshot.effective_config.permissions.allow, ["read"]);
    if let Some(path) = snapshot.transcript_path.or(snapshot.archive_path) {
        std::fs::remove_file(path).expect("remove VidaiMock child archive");
    }
}
