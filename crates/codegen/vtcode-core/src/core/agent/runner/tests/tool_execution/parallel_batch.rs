//! Runner tests for read-only tool batching and admission telemetry.

use super::*;
use tracing_subscriber::prelude::*;

#[tokio::test]
async fn readonly_tool_calls_emit_a_parallel_batch() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("first.txt"), "parallel_marker_first").expect("first file");
    fs::write(temp.path().join("second.txt"), "parallel_marker_second").expect("second file");

    let observed_batches = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(ToolBatchCapture(Arc::clone(&observed_batches)));
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let mut config = VTCodeConfig::default();
    config.tools.profile = ToolProfile::AdvancedVtCode;
    let mut runner = Box::pin(make_runner(&temp, config, "thread-readonly-parallel")).await;
    runner.enable_full_auto(&[tools::CODE_SEARCH.to_string()]).await;
    assert!(runner.is_valid_tool(tools::CODE_SEARCH).await);

    let tool_calls = vec![
        ToolCall::function(
            "call-search-first".to_string(),
            tools::CODE_SEARCH.to_string(),
            json!({ "query": "parallel_marker_first", "path": "first.txt" }).to_string(),
        ),
        ToolCall::function(
            "call-search-second".to_string(),
            tools::CODE_SEARCH.to_string(),
            json!({ "query": "parallel_marker_second", "path": "second.txt" }).to_string(),
        ),
    ];

    let mut agent_runtime =
        AgentRuntime::new(AgentSessionState::new("session-readonly-parallel".to_string(), 16, 4, 128_000), None, None);
    let mut recorder = ExecEventRecorder::new("thread-readonly-parallel", None, None);

    runner
        .execute_tool_call_batches(tool_calls, &mut agent_runtime, &mut recorder, "[batch]", false, false)
        .await
        .expect("tool execution should finish");

    let tool_outputs = agent_runtime
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
    for (call_id, expected_query) in [
        ("call-search-first", "parallel_marker_first"),
        ("call-search-second", "parallel_marker_second"),
    ] {
        let output = tool_outputs
            .iter()
            .find_map(|(id, output)| (*id == call_id).then_some(output))
            .expect("search result");
        assert_eq!(
            output.get("query").and_then(serde_json::Value::as_str),
            Some(expected_query),
            "unexpected output for {call_id}: {output}"
        );
    }

    let observed_batches = observed_batches.lock().clone();
    assert!(
        observed_batches.contains(&(2, true)),
        "two read-only calls should be recorded as a parallel batch; got {observed_batches:?}"
    );
}

#[tokio::test]
async fn parallel_code_search_denials_emit_parallel_batch() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join(".env"), "API_KEY=first-secret").expect("first sensitive file");
    fs::write(temp.path().join(".env.local"), "API_KEY=second-secret").expect("second sensitive file");
    let observed_batches = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(ToolBatchCapture(Arc::clone(&observed_batches)));
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let mut config = VTCodeConfig::default();
    config.tools.profile = ToolProfile::AdvancedVtCode;
    let mut runner = Box::pin(make_runner(&temp, config, "thread-readonly-sensitive")).await;
    runner.enable_full_auto(&[tools::CODE_SEARCH.to_string()]).await;
    assert!(runner.is_valid_tool(tools::CODE_SEARCH).await);

    let tool_calls = vec![
        ToolCall::function(
            "call-search-sensitive-first".to_string(),
            tools::CODE_SEARCH.to_string(),
            json!({ "query": "first_sensitive_query", "path": ".env" }).to_string(),
        ),
        ToolCall::function(
            "call-search-sensitive-second".to_string(),
            tools::CODE_SEARCH.to_string(),
            json!({ "query": "second_sensitive_query", "path": ".env.local" }).to_string(),
        ),
    ];

    let mut agent_runtime =
        AgentRuntime::new(AgentSessionState::new("session-readonly-sensitive".to_string(), 16, 4, 128_000), None, None);
    let mut recorder = ExecEventRecorder::new("thread-readonly-sensitive", None, None);

    runner
        .execute_tool_call_batches(tool_calls, &mut agent_runtime, &mut recorder, "[batch]", false, false)
        .await
        .expect("tool execution should finish");

    let tool_outputs = agent_runtime
        .state
        .messages
        .iter()
        .filter_map(|message| {
            let id = message.tool_call_id.as_ref()?;
            Some((id.as_str(), message.content.as_text().to_string()))
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_outputs.len(), 2);
    for call_id in ["call-search-sensitive-first", "call-search-sensitive-second"] {
        let output = tool_outputs
            .iter()
            .find_map(|(id, output)| (*id == call_id).then_some(output.as_str()))
            .expect("traversal rejection response");
        assert!(output.contains("sensitive"), "unexpected failure for {call_id}: {output}");
        assert!(!output.contains("\"results\""), "rejected call must not return search results: {output}");
    }

    let observed_batches = observed_batches.lock().clone();
    assert!(
        observed_batches.contains(&(2, true)),
        "two denied read-only calls should still be recorded as a parallel batch; got {observed_batches:?}"
    );
}

#[derive(Clone)]
struct ToolBatchCapture(Arc<Mutex<Vec<(u64, bool)>>>);

impl<S> tracing_subscriber::Layer<S> for ToolBatchCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = ToolBatchFields::default();
        event.record(&mut fields);
        if let (Some(tool_count), Some(parallelized)) = (fields.tool_count, fields.parallelized) {
            self.0.lock().push((tool_count, parallelized));
        }
    }
}

#[derive(Default)]
struct ToolBatchFields {
    tool_count: Option<u64>,
    parallelized: Option<bool>,
}

impl tracing::field::Visit for ToolBatchFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "tool_count" {
            self.tool_count = Some(value);
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "parallelized" {
            self.parallelized = Some(value);
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}
