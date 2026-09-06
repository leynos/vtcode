//! Working-history replay identity tests.

use super::*;

#[test]
fn working_history_code_search_replay_rejects_reused_patch_call_id() {
    let search_args = json!({"query": "Widget", "path": "src"});
    let shared_call_id = "call_0";
    let search_call = uni::Message::assistant_with_tools(
        "search".into(),
        vec![uni::ToolCall::function(
            shared_call_id.into(),
            tools::CODE_SEARCH.into(),
            serde_json::to_string(&search_args).unwrap(),
        )],
    );
    let search_result =
        uni::Message::tool_response(shared_call_id.into(), "{\"results\":[\"genuine search output\"]}".into());
    let patch = "*** Begin Patch\n*** Update File: src/widget.rs\n@@\n-Widget\n+Gadget\n*** End Patch\n";
    let patch_call = uni::Message::assistant_with_tools(
        "edit".into(),
        vec![uni::ToolCall::function(
            shared_call_id.into(),
            tools::APPLY_PATCH.into(),
            serde_json::to_string(&json!({"patch": patch})).unwrap(),
        )],
    );

    let mut successful_history = vec![
        search_call.clone(),
        search_result.clone(),
        patch_call.clone(),
        uni::Message::tool_response(
            shared_call_id.into(),
            json!({"success": true, "output": "patch output"}).to_string(),
        ),
    ];
    assert!(
        find_duplicate_in_history(&successful_history, tools::CODE_SEARCH, &search_args, Path::new("."),).is_none(),
        "a successful in-scope patch must invalidate the genuine earlier search result"
    );

    successful_history.pop();
    successful_history.push(uni::Message::tool_response(
        shared_call_id.into(),
        json!({"success": false, "error": "patch rejected", "output": "patch output"}).to_string(),
    ));
    assert_eq!(
        find_duplicate_in_history(&successful_history, tools::CODE_SEARCH, &search_args, Path::new("."),).as_deref(),
        Some("{\"results\":[\"genuine search output\"]}"),
        "a failed patch must preserve the earlier search without returning patch output"
    );
}
