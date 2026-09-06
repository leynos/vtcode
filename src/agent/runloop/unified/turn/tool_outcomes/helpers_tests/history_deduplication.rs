//! Working-history deduplication and invalidation tests.

use super::*;

#[test]
fn find_duplicate_in_history_matches_normalized_read() {
    use vtcode_core::llm::provider as uni;

    // find_duplicate_in_history uses read_normalized_signature_key, which
    // strips offset/limit for file reads. A later unrelated Assistant batch
    // must not obscure the earlier matching call and result pair.

    // Verify normalization: same file + different offset/limit → same key
    let key_a = read_normalized_signature_key(
        tools::UNIFIED_FILE,
        &json!({"action":"read","path":"src/lib.rs","offset":0,"limit":100}),
    );
    let key_b = read_normalized_signature_key(
        tools::UNIFIED_FILE,
        &json!({"action":"read","path":"src/lib.rs","offset":50,"limit":500}),
    );
    assert_eq!(key_a, key_b, "same file read with different offset/limit should normalize to the same key");

    // Verify: different file → different key
    let key_c = read_normalized_signature_key(
        tools::UNIFIED_FILE,
        &json!({"action":"read","path":"src/main.rs","offset":0,"limit":100}),
    );
    assert_ne!(key_a, key_c, "different files must produce different normalized keys");

    // Verify: code-search result limits remain distinct while filter ordering normalizes away.
    let s_key_a = read_normalized_signature_key(
        tools::CODE_SEARCH,
        &json!({"query":"Widget","path":"src","file_types":["rust","typescript"],"result_types":["text","definition"],"max_results":10}),
    );
    let s_key_b = read_normalized_signature_key(
        tools::CODE_SEARCH,
        &json!({"query":"Widget","path":"src","file_types":["typescript","rs"],"result_types":["definition","text"],"max_results":100}),
    );
    assert_ne!(s_key_a, s_key_b, "different effective limits must not share one code-search replay identity");

    // Verify: write NOT normalized
    let w_key_a = read_normalized_signature_key(
        tools::UNIFIED_FILE,
        &json!({"action":"write","path":"src/lib.rs","content":"old"}),
    );
    let w_key_b = read_normalized_signature_key(
        tools::UNIFIED_FILE,
        &json!({"action":"write","path":"src/lib.rs","content":"new"}),
    );
    assert_ne!(w_key_a, w_key_b, "writes must not be normalized away");

    // Verify: find_duplicate_in_history still works for EXACT match
    let mut history: Vec<uni::Message> = Vec::new();
    history.push(uni::Message::assistant_with_tools(
        "read".into(),
        vec![uni::ToolCall::function(
            "tc_exact".into(),
            tools::UNIFIED_FILE.into(),
            serde_json::to_string(&json!({"action":"read","path":"src/lib.rs","offset":0,"limit":100})).unwrap(),
        )],
    ));
    history.push(uni::Message {
        role: uni::MessageRole::Tool,
        content: uni::MessageContent::text("exact content".into()),
        tool_call_id: Some("tc_exact".into()),
        ..Default::default()
    });
    // Second pair (different file) so the scan finds A₀'s Tool after A₁:
    history.push(uni::Message::assistant_with_tools(
        "read other".into(),
        vec![uni::ToolCall::function(
            "tc_other".into(),
            tools::UNIFIED_FILE.into(),
            serde_json::to_string(&json!({"action":"read","path":"src/main.rs"})).unwrap(),
        )],
    ));
    history.push(uni::Message {
        role: uni::MessageRole::Tool,
        content: uni::MessageContent::text("other content".into()),
        tool_call_id: Some("tc_other".into()),
        ..Default::default()
    });

    let result = find_duplicate_in_history(
        &history,
        tools::UNIFIED_FILE,
        &json!({"action":"read","path":"src/lib.rs","offset":0,"limit":50}),
        Path::new("."),
    );
    assert_eq!(result.as_deref(), Some("exact content"));
}

#[test]
fn find_duplicate_in_history_respects_normalized_code_search_limit() {
    let original_args = json!({
        "query": "Widget",
        "path": "src",
        "file_types": ["rust", "typescript"],
        "result_types": ["text", "definition"],
        "max_results": 10
    });
    let history = vec![
        uni::Message::assistant_with_tools(
            "search".into(),
            vec![uni::ToolCall::function(
                "tc_search".into(),
                tools::CODE_SEARCH.into(),
                serde_json::to_string(&original_args).unwrap(),
            )],
        ),
        uni::Message {
            role: uni::MessageRole::Tool,
            content: uni::MessageContent::text("{\"results\":[]}".into()),
            tool_call_id: Some("tc_search".into()),
            ..Default::default()
        },
    ];

    let different_limit = find_duplicate_in_history(
        &history,
        tools::CODE_SEARCH,
        &json!({
            "query": "Widget",
            "path": "src",
            "file_types": ["typescript", "rs"],
            "result_types": ["definition", "text"],
            "max_results": 100
        }),
        Path::new("."),
    );

    assert_eq!(different_limit, None);

    let equivalent_default_history = vec![
        uni::Message::assistant_with_tools(
            "search".into(),
            vec![uni::ToolCall::function(
                "tc_default".into(),
                tools::CODE_SEARCH.into(),
                serde_json::to_string(&json!({
                    "query": "Widget",
                    "path": "src",
                    "max_results": 20
                }))
                .unwrap(),
            )],
        ),
        uni::Message {
            role: uni::MessageRole::Tool,
            content: uni::MessageContent::text("{\"results\":[1]}".into()),
            tool_call_id: Some("tc_default".into()),
            ..Default::default()
        },
    ];
    let reused = find_duplicate_in_history(
        &equivalent_default_history,
        tools::CODE_SEARCH,
        &json!({"query": " Widget ", "path": "src"}),
        Path::new("."),
    );
    assert_eq!(reused.as_deref(), Some("{\"results\":[1]}"));
}

#[test]
fn working_history_code_search_replay_stops_at_in_scope_mutation() {
    let search_args = json!({"query": "Widget", "path": "src"});
    let search_call = uni::Message::assistant_with_tools(
        "search".into(),
        vec![uni::ToolCall::function(
            "search_call".into(),
            tools::CODE_SEARCH.into(),
            serde_json::to_string(&search_args).unwrap(),
        )],
    );
    let search_result = uni::Message {
        role: uni::MessageRole::Tool,
        content: uni::MessageContent::text("{\"results\":[\"cached\"]}".into()),
        tool_call_id: Some("search_call".into()),
        ..Default::default()
    };
    let mutation = |path: &str, result: serde_json::Value| {
        let patch = format!("*** Begin Patch\n*** Update File: {path}\n@@\n-Widget\n+Gadget\n*** End Patch\n");
        vec![
            uni::Message::assistant_with_tools(
                "edit".into(),
                vec![uni::ToolCall::function(
                    "edit_call".into(),
                    tools::APPLY_PATCH.into(),
                    serde_json::to_string(&json!({"patch": patch})).unwrap(),
                )],
            ),
            uni::Message::tool_response("edit_call".into(), result.to_string()),
        ]
    };

    let mut in_scope_history = vec![search_call.clone(), search_result.clone()];
    in_scope_history.extend(mutation("src/widget.rs", json!({"success": true})));
    assert!(
        find_duplicate_in_history(&in_scope_history, tools::CODE_SEARCH, &search_args, Path::new("."),).is_none(),
        "editing src/widget.rs after searching src must force a fresh search"
    );

    let mut status_success_history = vec![search_call.clone(), search_result.clone()];
    status_success_history.extend(mutation("src/widget.rs", json!({"status": "success", "output": "patch applied"})));
    assert!(
        find_duplicate_in_history(&status_success_history, tools::CODE_SEARCH, &search_args, Path::new("."),).is_none(),
        "the established successful status shape must invalidate replay"
    );

    let mut unrelated_history = vec![search_call.clone(), search_result.clone()];
    unrelated_history.extend(mutation("tests/widget.rs", json!({"success": true})));
    assert_eq!(
        find_duplicate_in_history(&unrelated_history, tools::CODE_SEARCH, &search_args, Path::new("."),).as_deref(),
        Some("{\"results\":[\"cached\"]}"),
        "an unrelated edit may reuse the prior scoped search"
    );

    for failure in [
        json!({"success": false, "error": "patch rejected"}),
        json!({"error": {"message": "execution denied by policy"}}),
        json!({"failure_kind": "timeout"}),
        json!({"status": "failed"}),
        json!({"status": "denied"}),
        json!({"success": null}),
        json!({"output": "patch output without an outcome"}),
        json!(["non-object mutation output"]),
    ] {
        let mut failed_history = vec![search_call.clone(), search_result.clone()];
        failed_history.extend(mutation("src/widget.rs", failure));
        assert_eq!(
            find_duplicate_in_history(&failed_history, tools::CODE_SEARCH, &search_args, Path::new("."),).as_deref(),
            Some("{\"results\":[\"cached\"]}"),
            "a mutation without explicit positive success evidence must preserve reuse"
        );
    }

    let mut unexecuted_history = vec![search_call, search_result];
    let unexecuted_mutation = mutation("src/widget.rs", json!({"success": true}));
    unexecuted_history.push(unexecuted_mutation[0].clone());
    assert_eq!(
        find_duplicate_in_history(&unexecuted_history, tools::CODE_SEARCH, &search_args, Path::new("."),).as_deref(),
        Some("{\"results\":[\"cached\"]}"),
        "an unexecuted mutation call must preserve reuse"
    );
}

#[test]
fn mutation_tool_response_success_rejects_malformed_and_conflicting_shapes() {
    let response = |content: &str| uni::Message::tool_response("edit_call".into(), content.into());

    assert!(tool_response_is_success(&response(r#"{"success":true}"#)));
    assert!(tool_response_is_success(&response(r#"{"status":"success","output":"patch applied"}"#,)));

    for content in [
        "not json",
        "null",
        r#"{"success":null,"status":"success"}"#,
        r#"{"success":true,"status":"failed"}"#,
        r#"{"success":true,"failure_kind":"timeout"}"#,
        r#"{"success":true,"error":"execution denied"}"#,
    ] {
        assert!(
            !tool_response_is_success(&response(content)),
            "mutation outcome must not count as successful: {content}"
        );
    }
}

#[test]
fn duplicate_history_reuse_rejects_failed_results() {
    let args = json!({"query": "needle", "path": "src"});
    let call = || {
        uni::Message::assistant_with_tools(
            "search".into(),
            vec![uni::ToolCall::function(
                "search_call".into(),
                tools::CODE_SEARCH.into(),
                serde_json::to_string(&args).unwrap(),
            )],
        )
    };

    for failure in [
        r#"{"success":false,"output":"partial"}"#,
        r#"{"status":"timeout","output":"partial"}"#,
        r#"{"error":"permission denied"}"#,
        "Error: command failed",
        "timed out while reading",
        "failed to execute command",
        "denied by policy",
        "blocked until verification",
        "not executed",
    ] {
        let history = vec![
            call(),
            uni::Message::tool_response("search_call".into(), failure.into()),
        ];
        assert!(
            find_duplicate_in_history(&history, tools::CODE_SEARCH, &args, Path::new(".")).is_none(),
            "failed result must not be replayed: {failure}"
        );
    }

    for success in [r#"{"results":[]}"#, "[]", "plain successful output"] {
        let history = vec![
            call(),
            uni::Message::tool_response("search_call".into(), success.into()),
        ];
        assert_eq!(
            find_duplicate_in_history(&history, tools::CODE_SEARCH, &args, Path::new(".")).as_deref(),
            Some(success)
        );
    }
}
