//! Synchronous provider-protocol and normalisation cases for prompt
//! preservation.

use super::*;
use super::{assert_correlated, provider, task_message};

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
fn native_echo_with_plain_assistant_keeps_source_chronology_without_duplicates() {
    let original = task_message("Complete the task END-ECHO-ORDER".into(), "echo-order-task");
    let steering = task_message("Latest steering".into(), "echo-order-steering");
    let source = vec![
        original,
        Message::assistant("Interim report".into()),
        steering,
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

    assert_eq!(normalized, source[..3].to_vec());
    assert_eq!(
        normalized
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .count(),
        1
    );
    assert_correlated(&normalized);
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
