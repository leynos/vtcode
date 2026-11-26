//! Runner tests for denied tool admission and removed public tool names.

use super::*;

fn denied_unified_exec_response() -> LLMResponse {
    tool_call_response(
        tools::UNIFIED_EXEC,
        json!({
            "action": "run",
            "command": "echo diapason",
        }),
    )
}

fn denied_unified_exec_calls() -> Vec<ToolCall> {
    ["call-denied-a", "call-denied-b"]
        .into_iter()
        .map(|id| {
            ToolCall::function(
                id.to_string(),
                tools::UNIFIED_EXEC.to_string(),
                json!({
                    "action": "run",
                    "command": "echo diapason",
                })
                .to_string(),
            )
        })
        .collect()
}

async fn assert_sequential_denied_tools_are_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let thread_id = "thread-denied-tool";
    let session_id = "session-denied-tool";
    let mut runner = Box::pin(make_runner(&temp, DiapasonConfig::default(), thread_id)).await;
    runner
        .tool_registry
        .register_tool(crate::tools::registry::ToolRegistration::new(
            tools::UNIFIED_EXEC,
            CapabilityLevel::Bash,
            false,
            |_, _| Box::pin(std::future::pending::<anyhow::Result<serde_json::Value>>()),
        ))
        .await
        .expect("pending test executor should register");
    runner.enable_full_auto(&[tools::UNIFIED_FILE.to_string()]).await;

    let tool_calls = denied_unified_exec_calls();
    let tool_call_ids = tool_calls.iter().map(|call| call.id.clone()).collect::<Vec<_>>();

    let mut runtime = AgentRuntime::new(AgentSessionState::new(session_id.to_string(), 16, 4, 128_000), None, None);
    let mut recorder = ExecEventRecorder::new(thread_id, None, None);

    let mut execution =
        Box::pin(runner.execute_tool_call_batches(tool_calls, &mut runtime, &mut recorder, "[denied]", false, false));
    let waker = futures::task::noop_waker_ref();
    let mut context = std::task::Context::from_waker(waker);
    match Future::poll(execution.as_mut(), &mut context) {
        std::task::Poll::Ready(result) => result.expect("tool execution should finish"),
        std::task::Poll::Pending => {
            panic!("denied tool admission must complete before the pending executor can be polled")
        }
    }
    drop(execution);

    let expected_denial = format!("Tool execution denied: {}", tools::UNIFIED_EXEC);
    assert!(
        runtime.state.warnings.iter().any(|warning| warning == &expected_denial),
        "denied tool warning should be recorded: {:?}",
        runtime.state.warnings
    );
    assert!(!runtime.state.executed_commands.iter().any(|tool| tool == tools::UNIFIED_EXEC));

    let events = recorder.into_events();
    let invocation_count = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                ThreadEvent::ItemCompleted(ItemCompletedEvent { item })
                    if matches!(&item.details, ThreadItemDetails::ToolInvocation(_))
            )
        })
        .count();
    assert_eq!(invocation_count, 2);
    for tool_call_id in &tool_call_ids {
        assert_single_failed_output(&events, tool_call_id);
    }
}

#[tokio::test]
async fn denied_tool_call_emits_one_failed_output_for_runtime_invocation() {
    let temp = TempDir::new().expect("tempdir");
    let mut runner = Box::pin(make_runner(&temp, DiapasonConfig::default(), "thread-denied-tool-output")).await;
    runner.enable_full_auto(&[tools::UNIFIED_FILE.to_string()]).await;

    let response = denied_unified_exec_response();
    let provider = QueuedProvider::new(vec![response]);
    let mut provider_box: Box<dyn LLMProvider> = Box::new(provider);

    let mut runtime =
        AgentRuntime::new(AgentSessionState::new("session-denied".to_string(), 16, 4, 128_000), None, None);
    let tool_calls = run_single_tool_call_turn(&mut runtime, &mut provider_box).await;
    let tool_call_id = tool_calls[0].id.clone();
    let mut recorder = ExecEventRecorder::new("thread-denied-tool-output", None, None);
    recorder.record_thread_events(runtime.take_emitted_events());

    runner
        .execute_tool_call_batches(tool_calls, &mut runtime, &mut recorder, "[single]", false, false)
        .await
        .expect("tool execution should finish");

    let events = recorder.into_events();
    assert_single_failed_output(&events, &tool_call_id);
}

#[tokio::test]
async fn sequential_denied_tool_calls_are_rejected_without_execution() {
    assert_sequential_denied_tools_are_rejected().await;
}

#[tokio::test]
async fn removed_public_tool_names_are_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let mut runner = Box::pin(make_runner(&temp, DiapasonConfig::default(), "thread-removed-public-tools")).await;
    assert!(!runner.is_valid_tool(tools::LIST_FILES).await);
    assert!(!runner.is_valid_tool(tools::UNIFIED_SEARCH).await);

    let tool_calls = vec![
        ToolCall::function(
            "call-list".to_string(),
            tools::LIST_FILES.to_string(),
            json!({
                "path": "notes",
            })
            .to_string(),
        ),
        ToolCall::function(
            "call-search".to_string(),
            tools::UNIFIED_SEARCH.to_string(),
            json!({
                "action": "grep",
                "path": "notes",
                "pattern": "hello",
            })
            .to_string(),
        ),
    ];

    let mut runtime = AgentRuntime::new(
        AgentSessionState::new("session-removed-public-tools".to_string(), 16, 4, 128_000),
        None,
        None,
    );
    let mut recorder = ExecEventRecorder::new("thread-removed-public-tools", None, None);

    runner
        .execute_tool_call_batches(tool_calls, &mut runtime, &mut recorder, "[batch]", false, false)
        .await
        .expect("tool execution should finish");

    let tool_outputs = runtime
        .state
        .messages
        .iter()
        .filter_map(|message| {
            let id = message.tool_call_id.as_ref()?;
            let output = serde_json::from_str::<serde_json::Value>(&message.content.as_text()).ok()?;
            Some((id.as_str(), output))
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_outputs.len(), 2);

    let list_output = tool_outputs
        .iter()
        .find_map(|(id, output)| (*id == "call-list").then_some(output))
        .expect("list_files rejection output");
    assert_eq!(list_output.as_str(), Some("Tool execution denied: list_files"));

    let search_output = tool_outputs
        .iter()
        .find_map(|(id, output)| (*id == "call-search").then_some(output))
        .expect("search_dispatch rejection output");
    let expected_search_denial = format!("Tool execution denied: {}", tools::UNIFIED_SEARCH);
    assert_eq!(search_output.as_str(), Some(expected_search_denial.as_str()));

    let events = recorder.into_events();
    let list_item_id = completed_tool_invocation_item_id(&events, "call-list").expect("list invocation");
    let search_item_id = completed_tool_invocation_item_id(&events, "call-search").expect("search invocation");
    assert_eq!(completed_tool_output_count(&events, "call-list", ToolCallStatus::Failed, &list_item_id), 1);
    assert_eq!(completed_tool_output_count(&events, "call-search", ToolCallStatus::Failed, &search_item_id), 1);
}
