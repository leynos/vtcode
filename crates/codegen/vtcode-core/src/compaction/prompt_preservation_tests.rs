//! Regression coverage for complete task instructions, bounded protocol work,
//! and continuity summaries across repeated compaction and model-budget limits.

use super::*;
use crate::llm::provider::{LLMError, LLMResponse};
use async_trait::async_trait;
use std::sync::Mutex;
use vtcode_commons::llm::ToolCall;
use vtcode_commons::message_metadata::MessageMetadata;

struct RecordingProvider {
    context_size: usize,
    requests: Mutex<Vec<LLMRequest>>,
}

#[async_trait]
impl LLMProvider for RecordingProvider {
    fn name(&self) -> &str {
        "prompt-preservation"
    }
    fn supported_models(&self) -> Vec<String> {
        vec!["test".into()]
    }
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }
    fn effective_context_size(&self, _model: &str) -> usize {
        self.context_size
    }
    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.requests.lock().expect("requests lock").push(request);
        Ok(LLMResponse::new("test", "Goal: preserve original instructions. Recent work: checked boundary."))
    }
}

fn provider(context_size: usize) -> RecordingProvider {
    RecordingProvider { context_size, requests: Mutex::new(Vec::new()) }
}

fn task_message(content: String, intent_id: &str) -> Message {
    let mut message = Message::user(content);
    message.metadata = Some(MessageMetadata::user_input(1, message.estimate_tokens()).with_intent_id(intent_id));
    message
}

fn append_work(history: &mut Vec<Message>, target_len: usize) {
    while history.len() + 1 < target_len {
        let call_id = format!("boundary-{}", history.len());
        history.push(Message::assistant_with_tools(
            "Inspecting boundary".into(),
            vec![ToolCall::function(call_id.clone(), "read_file".into(), "{}".into())],
        ));
        history
            .push(Message::tool_response(call_id, format!("Recent work: checked boundary. {}", "output ".repeat(400))));
    }
    if history.len() < target_len {
        history.push(Message::assistant("Boundary checked".into()));
    }
}

fn assert_correlated(history: &[Message]) {
    for (index, message) in history.iter().enumerate() {
        if let Some(calls) = message.tool_calls.as_ref() {
            let result_ids = history[index + 1..]
                .iter()
                .take_while(|next| next.role == MessageRole::Tool)
                .filter_map(|next| next.tool_call_id.as_deref())
                .collect::<Vec<_>>();
            assert_eq!(result_ids.len(), calls.len());
            for call in calls {
                assert!(result_ids.contains(&call.id.as_str()));
            }
        }
        if let Some(result_id) = message.tool_call_id.as_deref() {
            assert!(
                history[..index]
                    .iter()
                    .rev()
                    .find(|prior| prior.role != MessageRole::Tool)
                    .and_then(|prior| prior.tool_calls.as_ref())
                    .is_some_and(|calls| calls.iter().any(|call| call.id == result_id))
            );
        }
    }
}

#[tokio::test]
async fn repeated_compaction_retains_complete_task_after_152_and_457_messages() {
    let provider = provider(200_000);
    let original = task_message(
        format!("Implement the complete task. {}\nEND-OF-TASK-SENTINEL", "requirement ".repeat(1_220)),
        "original-task",
    );
    let config = CompactionConfig {
        always_summarize: true,
        retained_user_message_tokens: 8,
        ..Default::default()
    };
    let mut history = vec![original.clone()];
    for target_len in [152, 457] {
        append_work(&mut history, target_len);
        assert_eq!(history.len(), target_len);
        history = compact_history_manual(&provider, "test", &history, &config, &Default::default())
            .await
            .expect("compaction")
            .0;
        let users = history
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .collect::<Vec<_>>();
        assert_eq!(users, vec![&original]);
        assert_correlated(&history);
        assert!(
            history.iter().map(Message::estimate_tokens).sum::<usize>()
                <= compaction_history_budget(&provider, "test").unwrap()
        );
    }
    for request in provider.requests.lock().expect("requests lock").iter() {
        let prompt = request.messages[0].content.as_text();
        assert!(prompt.contains("END-OF-TASK-SENTINEL"));
        assert!(prompt.contains("Recent work: checked boundary."), "summary must see active-turn work");
    }
}

#[tokio::test]
async fn prompt_larger_than_continuity_budget_survives_when_model_can_admit_it() {
    let provider = provider(200_000);
    let original = task_message(format!("{}END-LARGE-TASK", "constraint boundary ".repeat(12_000)), "large-task");
    assert!(original.estimate_tokens() > CONTINUITY_TAIL_TARGET_TOKENS);
    let mut history = vec![original.clone()];
    append_work(&mut history, 20);
    let config = CompactionConfig { always_summarize: true, ..Default::default() };
    let compacted = compact_history(&provider, "test", &history, &config)
        .await
        .expect("large task fits model");
    assert_eq!(compacted.iter().find(|message| message.role == MessageRole::User), Some(&original));
    assert!(
        compacted.iter().map(Message::estimate_tokens).sum::<usize>()
            <= compaction_history_budget(&provider, "test").unwrap()
    );
}

#[tokio::test]
async fn impossible_task_fails_before_provider_call_without_mutating_history() {
    let provider = provider(4_096);
    let history = vec![task_message("must keep every instruction ".repeat(2_000), "oversized")];
    let before = history.clone();
    let error = compact_history_manual(&provider, "test", &history, &Default::default(), &Default::default())
        .await
        .expect_err("task cannot fit");
    assert!(error.to_string().contains("user instructions were left unchanged"));
    assert_eq!(history, before);
    assert!(provider.requests.lock().expect("requests lock").is_empty());
}

#[tokio::test]
async fn original_task_and_latest_steering_keep_identity_and_chronology() {
    let provider = provider(32_768);
    let original =
        task_message(format!("Original acceptance criteria {}END-ORIGINAL", "requirement ".repeat(800)), "original");
    let steering = task_message("Keep the task scope and verify the latest boundary".into(), "steering");
    let mut history = vec![original.clone()];
    append_work(&mut history, 153);
    history.push(steering.clone());
    append_work(&mut history, 300);
    let config = CompactionConfig { always_summarize: true, ..Default::default() };
    let compacted = compact_history(&provider, "test", &history, &config).await.expect("compaction");
    let users = compacted
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .collect::<Vec<_>>();
    assert_eq!(users, vec![&original, &steering]);
    assert_correlated(&compacted);
    assert!(
        compacted.iter().map(Message::estimate_tokens).sum::<usize>()
            <= compaction_history_budget(&provider, "test").unwrap()
    );
    let requests = provider.requests.lock().expect("requests lock");
    assert!(requests[0].messages[0].estimate_tokens() < provider.context_size, "summary request must fit too");
}

#[test]
fn native_normalization_restores_full_source_instructions_with_duplicate_intents() {
    let original = task_message("Keep the full instructions END-SENTINEL".into(), "original");
    let first = task_message("same steering".into(), "first-steering");
    let second = task_message("same steering".into(), "second-steering");
    let source = vec![original.clone(), first.clone(), second.clone()];
    let native = vec![
        Message::system("native summary".into()),
        Message::user("Keep the full".into()),
    ];
    let normalized = normalize_provider_compacted_history(native, &source);
    assert_eq!(&normalized[1..], source.as_slice());
}

#[test]
fn bounded_parallel_tool_previews_retain_ids_and_are_labelled() {
    let mut history = vec![
        Message::user("task".into()),
        Message::assistant_with_tools(
            "working".into(),
            vec![
                ToolCall::function("one".into(), "run".into(), "{}".into()),
                ToolCall::function("two".into(), "run".into(), "{}".into()),
            ],
        ),
        Message::tool_response("two".into(), "large output ".repeat(10_000)),
        Message::tool_response("one".into(), "large output ".repeat(10_000)),
    ];
    history.extend([Message::assistant("latest".into())]);
    let tail = bounded_protocol_group(&history, 1_024);
    assert_correlated(&tail);
    assert!(tail.iter().map(Message::estimate_tokens).sum::<usize>() <= 1_024);
    assert!(
        tail.iter()
            .filter(|message| message.role == MessageRole::Tool)
            .all(|message| message.content.as_text().starts_with("[Compaction preview:"))
    );
    assert_eq!(tail.iter().filter(|message| message.role == MessageRole::Tool).count(), 2);
}

#[rstest::rstest]
#[case::duplicate_calls(&["one", "one"], vec![], 1)]
#[case::empty_call_id(&[""], vec![], 1)]
#[case::unknown_result(&["one"], vec![Message::tool_response("other".into(), "output".into())], 1)]
#[case::missing_result_id(&["one"], vec![{ let mut result = Message::tool_response("one".into(), "output".into()); result.tool_call_id = None; result }], 1)]
#[case::repeated_result(&["one", "two"], vec![Message::tool_response("one".into(), "output".into()), Message::tool_response("one".into(), "duplicate".into())], 1)]
#[case::non_tool_interrupt(&["one"], vec![Message::assistant("interrupted".into())], 1)]
#[case::out_of_order_results(&["one", "two"], vec![Message::tool_response("two".into(), "second".into()), Message::tool_response("one".into(), "first".into())], 4)]
#[case::legacy_orphan(&[], vec![Message::tool_response("legacy".into(), "output".into())], 3)]
fn protocol_prefix_preserves_only_complete_correlatable_batches(
    #[case] call_ids: &[&str],
    #[case] results: Vec<Message>,
    #[case] expected_prefix: usize,
) {
    let mut history = vec![
        Message::user("task".into()),
        Message::assistant_with_tools(
            "working".into(),
            call_ids
                .iter()
                .map(|id| ToolCall::function((*id).into(), "run".into(), "{}".into()))
                .collect(),
        ),
    ];
    history.extend(results);

    assert_eq!(complete_protocol_prefix(&history), expected_prefix);
}

#[tokio::test]
async fn hierarchical_summary_includes_active_work_and_keeps_complete_task() {
    let provider = provider(32_768);
    let original = task_message("Complete task END-HIERARCHICAL-TASK".into(), "hierarchical-task");
    let mut history = vec![original.clone()];
    append_work(&mut history, 60);
    let config = CompactionConfig { hierarchical: true, ..Default::default() };
    let compacted = compact_history_manual(&provider, "test", &history, &config, &Default::default())
        .await
        .expect("hierarchical compaction")
        .0;
    assert_eq!(compacted.iter().find(|message| message.role == MessageRole::User), Some(&original));
    let requests = provider.requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages[0]
            .content
            .as_text()
            .contains("Recent work: checked boundary.")
    );
}

#[test]
fn final_context_bound_fails_closed_for_protected_instructions() {
    let provider = provider(4_096);
    let original = task_message("preserve instruction ".repeat(2_000), "task");
    let compacted = vec![Message::system("summary".into()), original.clone()];
    let error =
        bound_compacted_history_to_context(compacted, &provider, "test").expect_err("protected prompt cannot fit");
    assert!(error.to_string().contains("exceeding the model context length budget"));
    assert!(original.content.as_text().ends_with("preserve instruction "));
}

#[test]
fn native_noop_preserves_repeated_reads_protocol_order_and_user_metadata() {
    let provider = provider(32_768);
    let mut history = vec![Message::system("session instructions".into())];
    for (call_id, result) in [("older-read", "older contents"), ("newer-read", "newer contents")] {
        history.push(Message::assistant_with_tools(
            "Inspecting the same file".into(),
            vec![ToolCall::function(
                call_id.into(),
                "read_file".into(),
                r#"{"path":"src/lib.rs","start_line":1,"end_line":40}"#.into(),
            )],
        ));
        history.push(Message::tool_response_with_origin(call_id.into(), result.into(), "read_file".into()));
    }
    history.push(task_message("Complete task END-NOOP-TASK".into(), "original-task"));
    history.push(Message::assistant("Latest completed work".into()));
    history.push(task_message("Keep the latest steering".into(), "latest-steering"));
    history.push(Message::assistant("Acknowledged".into()));

    let normalized = normalize_provider_compacted_history(history.clone(), &history);
    let bounded = bound_compacted_history_to_context(normalized, &provider, "test").expect("history fits model budget");

    assert_eq!(bounded, history, "an unchanged provider response must remain an exact no-op");
    assert_correlated(&bounded);
}

#[test]
fn native_echo_drops_an_incomplete_parallel_tool_batch() {
    let original = task_message("Complete both checks END-PARALLEL-TASK".into(), "parallel-task");
    let source = vec![
        Message::system("session instructions".into()),
        original.clone(),
        Message::assistant_with_tools(
            "Checking both files".into(),
            vec![
                ToolCall::function("first-check".into(), "read_file".into(), "{}".into()),
                ToolCall::function("second-check".into(), "read_file".into(), "{}".into()),
            ],
        ),
        Message::tool_response("first-check".into(), "First check completed".into()),
    ];

    let normalized = normalize_provider_compacted_history(source.clone(), &source);

    assert_eq!(normalized, vec![source[0].clone(), original]);
    assert_correlated(&normalized);
}

#[tokio::test]
async fn hierarchical_detail_survives_when_context_requires_bounding() {
    let provider = provider(4_096);
    let original = task_message("Complete task END-BOUNDED-HIERARCHICAL-TASK".into(), "bounded-hierarchical-task");
    let mut history = vec![original.clone()];
    append_work(&mut history, 30);
    let history_budget = compaction_history_budget(&provider, "test").expect("bounded model");
    assert!(history.iter().map(Message::estimate_tokens).sum::<usize>() > history_budget);
    let config = CompactionConfig { hierarchical: true, ..Default::default() };

    let compacted = compact_history_manual(&provider, "test", &history, &config, &Default::default())
        .await
        .expect("hierarchical compaction fits model")
        .0;

    let summaries = compacted
        .iter()
        .take_while(|message| message.role == MessageRole::System)
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 2, "context bounding must retain both summary tiers");
    assert!(summaries[0].content.as_text().starts_with(ABSTRACT_PREFIX));
    assert!(summaries[1].content.as_text().starts_with(DETAIL_PREFIX));
    assert!(summaries[1].content.as_text().contains("Recent work: checked boundary."));
    assert!(summaries.iter().map(|message| message.estimate_tokens()).sum::<usize>() <= 4_096);
    assert_eq!(compacted.iter().find(|message| message.role == MessageRole::User), Some(&original));
    assert!(compacted.iter().map(Message::estimate_tokens).sum::<usize>() <= history_budget);
    assert_correlated(&compacted);
}
