use super::super::types::ToolCallResult;
use serde_json::json;
use std::collections::HashSet;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::provider::{Message, MessageRole, ToolCall};

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RecoveryReport {
    pub(super) repaired_calls: usize,
}

pub(super) fn stage_tool_calls(messages: &mut Vec<Message>, assistant: Message) -> Vec<String> {
    let calls = assistant.tool_calls.as_deref().unwrap_or_default().to_vec();
    let call_ids = calls.iter().map(|call| call.id.clone()).collect();
    messages.push(assistant);
    messages.extend(calls.iter().map(incomplete_tool_result));
    call_ids
}

pub(super) fn replace_tool_results(messages: &mut Vec<Message>, results: &[ToolCallResult]) -> usize {
    let mut finalized = 0;
    for result in results {
        if let Some(message) = messages.iter_mut().find(|message| {
            message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(&result.tool_call_id)
        }) {
            *message = Message::tool_response(result.tool_call_id.clone(), result.llm_response.clone());
        } else {
            messages.push(Message::tool_response(result.tool_call_id.clone(), result.llm_response.clone()));
        }
        finalized += 1;
    }
    finalized
}

pub(super) fn stage_thread_tool_calls(thread: &ThreadRuntimeHandle, assistant: Message) -> Vec<String> {
    let mut messages = thread.messages();
    let call_ids = stage_tool_calls(&mut messages, assistant);
    thread.replace_messages(messages);
    call_ids
}

pub(super) fn replace_thread_tool_results(thread: &ThreadRuntimeHandle, results: &[ToolCallResult]) -> usize {
    let mut messages = thread.messages();
    let finalized = replace_tool_results(&mut messages, results);
    thread.replace_messages(messages);
    finalized
}

pub(super) fn repair_unresolved_tool_calls(messages: &mut Vec<Message>) -> RecoveryReport {
    let mut repaired_calls = 0;
    let mut message_index = 0;

    while message_index < messages.len() {
        let Some(message) = messages.get(message_index) else {
            break;
        };
        let calls = message
            .tool_calls
            .as_deref()
            .filter(|_| message.role == MessageRole::Assistant)
            .unwrap_or_default()
            .to_vec();
        if calls.is_empty() {
            message_index += 1;
            continue;
        }

        let mut group_end = message_index + 1;
        let mut completed_ids = HashSet::with_capacity(calls.len());
        while let Some(message) = messages.get(group_end).filter(|message| message.role == MessageRole::Tool) {
            if let Some(call_id) = message.tool_call_id.as_deref() {
                let _ = completed_ids.insert(call_id.to_string());
            }
            group_end += 1;
        }

        let missing = calls
            .iter()
            .filter(|call| !completed_ids.contains(call.id.as_str()))
            .map(incomplete_tool_result)
            .collect::<Vec<_>>();
        repaired_calls += missing.len();
        let missing_count = missing.len();
        drop(messages.splice(group_end..group_end, missing));
        message_index = group_end + missing_count;
    }

    RecoveryReport { repaired_calls }
}

fn incomplete_tool_result(call: &ToolCall) -> Message {
    let tool_name = call.function.as_ref().map_or("unknown", |function| function.name.as_str());
    let content = json!({
        "status": "incomplete",
        "tool": tool_name,
        "message": "Tool execution did not reach a durable terminal result. Its side effects are uncertain.",
        "replayed": false,
        "state": "uncertain",
        "next_action": "Verify the current workspace state, then resubmit the tool call only if the requested work is still needed."
    })
    .to_string();
    Message::tool_response(call.id.clone(), content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::btree_map;
    use proptest::prelude::*;
    use std::collections::BTreeSet;
    use vtcode_core::llm::provider::MessageContent;
    use vtcode_safety::audit_log::ToolAuditStatus;

    fn call(id: &str) -> ToolCall {
        ToolCall::function(id.to_string(), "apply_patch".to_string(), "{}".to_string())
    }

    fn assistant(calls: impl IntoIterator<Item = ToolCall>) -> Message {
        Message::assistant_with_tools(String::new(), calls.into_iter().collect())
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message::tool_response(id.to_string(), content.to_string())
    }

    fn tool_ids_after(messages: &[Message], assistant_index: usize) -> Vec<String> {
        messages
            .iter()
            .skip(assistant_index + 1)
            .take_while(|message| message.role == MessageRole::Tool)
            .filter_map(|message| message.tool_call_id.clone())
            .collect()
    }

    fn content_for<'a>(messages: &'a [Message], call_id: &str) -> Option<&'a str> {
        messages
            .iter()
            .find(|message| message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(call_id))
            .and_then(|message| match &message.content {
                MessageContent::Text(content) => Some(content.as_str()),
                MessageContent::Parts(_) => None,
            })
    }

    #[test]
    fn unresolved_history_is_not_closed_without_repair() {
        let messages = vec![assistant([call("call-1")]), Message::user("continue".to_string())];

        assert!(tool_ids_after(&messages, 0).is_empty());
    }

    #[test]
    fn repair_inserts_incomplete_result_before_later_messages() {
        let mut messages = vec![assistant([call("call-1")]), Message::user("continue".to_string())];

        let report = repair_unresolved_tool_calls(&mut messages);

        assert_eq!(report.repaired_calls, 1);
        assert_eq!(tool_ids_after(&messages, 0), ["call-1"]);
        assert_eq!(messages[2].role, MessageRole::User);
        let content = content_for(&messages, "call-1").expect("recovery result should have text content");
        let recovery: serde_json::Value = serde_json::from_str(content).expect("recovery result should be JSON");
        assert_eq!(recovery["status"], "incomplete");
        assert_eq!(recovery["replayed"], false);
        let next_action = recovery["next_action"]
            .as_str()
            .expect("recovery result should have a next action")
            .to_ascii_lowercase();
        assert!(next_action.contains("verify"));
        assert!(next_action.contains("resubmit"));
    }

    #[test]
    fn complete_history_is_preserved() {
        let mut messages = vec![assistant([call("call-1")]), tool_result("call-1", "real result")];
        let expected = messages.clone();

        let report = repair_unresolved_tool_calls(&mut messages);

        assert_eq!(report.repaired_calls, 0);
        assert_eq!(messages, expected);
    }

    #[test]
    fn stage_writes_one_incomplete_result_per_call() {
        let mut messages = vec![Message::user("edit files".to_string())];

        let call_ids = stage_tool_calls(&mut messages, assistant([call("call-1"), call("call-2")]));

        assert_eq!(call_ids, ["call-1", "call-2"]);
        assert_eq!(tool_ids_after(&messages, 1), ["call-1", "call-2"]);
    }

    #[test]
    fn successful_results_replace_incomplete_results() {
        let mut messages = Vec::new();
        drop(stage_tool_calls(&mut messages, assistant([call("call-1")])));
        let results = [ToolCallResult {
            tool_call_id: "call-1".to_string(),
            llm_response: "real result".to_string(),
            audit_status: ToolAuditStatus::Success,
        }];

        let replaced = replace_tool_results(&mut messages, &results);

        assert_eq!(replaced, 1);
        assert_eq!(content_for(&messages, "call-1"), Some("real result"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1_024))]

        #[test]
        fn repair_is_complete_idempotent_and_preserves_real_results(
            cases in btree_map("[a-z][a-z0-9]{0,7}", (any::<bool>(), "[ -~]{0,32}"), 1..9),
        ) {
            let calls = cases.keys().map(|id| call(id)).collect::<Vec<_>>();
            let mut messages = vec![Message::system("before".to_string()), assistant(calls)];
            let expected_real = cases
                .iter()
                .filter(|(_, (completed, _))| *completed)
                .map(|(id, (_, content))| (id.clone(), content.clone()))
                .collect::<Vec<_>>();
            for (id, content) in &expected_real {
                messages.push(tool_result(id, content));
            }
            messages.push(Message::user("after".to_string()));
            let expected_missing = cases.len() - expected_real.len();

            let first = repair_unresolved_tool_calls(&mut messages);
            let once = messages.clone();
            let second = repair_unresolved_tool_calls(&mut messages);

            prop_assert_eq!(first.repaired_calls, expected_missing);
            prop_assert_eq!(second.repaired_calls, 0);
            prop_assert_eq!(&messages, &once);

            let actual_ids = tool_ids_after(&messages, 1).into_iter().collect::<BTreeSet<_>>();
            let expected_ids = cases.keys().cloned().collect::<BTreeSet<_>>();
            prop_assert_eq!(actual_ids, expected_ids);
            for (id, expected_content) in expected_real {
                prop_assert_eq!(content_for(&messages, &id), Some(expected_content.as_str()));
            }
        }
    }
}
