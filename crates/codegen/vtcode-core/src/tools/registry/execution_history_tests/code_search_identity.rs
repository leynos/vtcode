//! Code-search loop identity and replay-normalization tests.

use super::*;

#[test]
fn code_search_loop_identity_normalizes_query_filters_and_limit() {
    let history = ToolExecutionHistory::new(10);
    history.set_loop_detection_limits(5, 2);

    let args = json!({
        "query": "exec_only_policy",
        "path": "crates/codegen/vtcode-core/src/core/agent/runner/tests.rs",
        "file_types": ["rust"],
        "result_types": ["definition", "usage"],
        "max_results": 5
    });

    // With MIN_READONLY_IDENTICAL_LIMIT=2, two successful calls with the
    // same limit-insensitive loop identity are enough to trigger detection.
    for _ in 0..2 {
        history.add_record(ToolExecutionRecord::success(
            tools::CODE_SEARCH.to_string(),
            tools::CODE_SEARCH.to_string(),
            false,
            None,
            args.clone(),
            json!({"query": "exec_only_policy", "filters": {}, "results": [], "returned": 0, "truncated": false, "hints": []}),
            make_snapshot(),
            None,
            None,
            None,
            None,
            false,
        ));
    }

    let mut equivalent_args = args.clone();
    equivalent_args["max_results"] = json!(100);
    let loop_result = history.detect_loop(tools::CODE_SEARCH, &equivalent_args);
    assert!(
        loop_result.detected,
        "two loop-equivalent calls should trigger detection despite differing max_results"
    );

    // A third loop-equivalent call increases the repeat count.
    history.add_record(ToolExecutionRecord::success(
        tools::CODE_SEARCH.to_string(),
        tools::CODE_SEARCH.to_string(),
        false,
        None,
        args.clone(),
        json!({"query": "exec_only_policy", "filters": {}, "results": [], "returned": 0, "truncated": false, "hints": []}),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let loop_result = history.detect_loop(tools::CODE_SEARCH, &args);
    assert!(loop_result.detected);
    assert_eq!(loop_result.repeat_count, 3);
    assert_eq!(loop_result.tool_name, tools::CODE_SEARCH);

    for changed in ["query", "path", "file_types", "result_types"] {
        let mut changed_args = args.clone();
        changed_args[changed] = match changed {
            "query" => json!("different"),
            "path" => json!("vtcode-core/tests"),
            "file_types" => json!(["python"]),
            "result_types" => json!(["text"]),
            _ => unreachable!(),
        };
        assert!(!history.detect_loop(tools::CODE_SEARCH, &changed_args).detected);
    }
}

#[test]
fn code_search_replay_matches_omitted_path_and_default_limit() {
    let history = ToolExecutionHistory::new(10);
    let cached_args = json!({"query": "ToolRegistry"});
    let cached_result = json!({"results": ["cached default search"]});

    history.add_record(ToolExecutionRecord::success(
        tools::CODE_SEARCH.to_string(),
        tools::CODE_SEARCH.to_string(),
        false,
        None,
        cached_args,
        cached_result.clone(),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let replayed = history.find_recent_successful_by_read_target(
        tools::CODE_SEARCH,
        &json!({"query": "ToolRegistry", "max_results": 20}),
        Duration::from_secs(60),
    );

    assert_eq!(replayed, Some(cached_result));
}

proptest! {
    /// Equivalent omitted, null, and explicit search defaults reuse a cached result.
    #[test]
    fn code_search_replay_normalizes_omitted_and_explicit_defaults(
        query in "[A-Za-z][A-Za-z0-9_]{0,20}",
    ) {
        let omitted_defaults = json!({"query": format!("  {query}\t")});
        let null_path = json!({"query": format!("  {query}\t"), "path": null});
        let explicit_defaults = json!({
            "query": &query,
            "path": ".",
            "result_types": ["definition", "usage", "text", "path"],
            "max_results": 20,
        });

        prop_assert!(
            replays_code_search(omitted_defaults, &explicit_defaults),
            "omitted defaults must replay against their explicit effective values"
        );
        prop_assert!(
            replays_code_search(null_path, &explicit_defaults),
            "a null path must normalize to the explicit root path"
        );
    }
}

proptest! {
    /// Normalized filters replay only when every effective search dimension agrees.
    #[test]
    fn code_search_replay_uses_each_effective_normalized_filter(
        query in "[A-Za-z][A-Za-z0-9_]{0,20}",
        path in prop_oneof![
            Just(None::<String>),
            "[a-z][a-z0-9_/]{0,20}".prop_map(Some),
        ],
        file_types in prop::sample::subsequence(
            vec!["rust", "python", "typescript"],
            1..=3,
        ),
        result_types in prop::sample::subsequence(
            vec!["definition", "usage", "text", "path"],
            1..=4,
        ),
        max_results in 1_usize..=100,
    ) {
        let cached_args = json!({
            "query": &query,
            "path": path.as_deref(),
            "file_types": &file_types,
            "result_types": &result_types,
            "max_results": max_results,
        });
        let whitespace_path = path.as_ref().map(|value| format!("  {value}\t"));
        let equivalent_file_types: Vec<&str> = file_types
            .iter()
            .rev()
            .map(|file_type| match *file_type {
                "rust" => " .rs ",
                "python" => " py3 ",
                "typescript" => " .ts ",
                _ => *file_type,
            })
            .collect();
        let equivalent_result_types: Vec<&str> = result_types.iter().rev().copied().collect();
        let equivalent_args = json!({
            "query": format!("\n{query}  "),
            "path": whitespace_path,
            "file_types": equivalent_file_types,
            "result_types": equivalent_result_types,
            "max_results": max_results,
        });

        prop_assert!(
            replays_code_search(cached_args.clone(), &equivalent_args),
            "whitespace and equivalent filter spellings must replay"
        );

        let mut changed_file_types = file_types.clone();
        if let Some(index) = changed_file_types
            .iter()
            .position(|file_type| *file_type == "rust")
        {
            changed_file_types.remove(index);
            if changed_file_types.is_empty() {
                changed_file_types.push("python");
            }
        } else {
            changed_file_types.push("rust");
        }
        let mut changed_result_types = result_types.clone();
        if let Some(index) = changed_result_types
            .iter()
            .position(|result_type| *result_type == "definition")
        {
            changed_result_types.remove(index);
            if changed_result_types.is_empty() {
                changed_result_types.push("usage");
            }
        } else {
            changed_result_types.push("definition");
        }
        let different_max_results = if max_results == 100 { 1 } else { max_results + 1 };
        let different_path = if path.as_deref() == Some("tests") { "src" } else { "tests" };

        for changed_args in [
            json!({
                "query": format!("{query}_changed"),
                "path": path.as_deref(),
                "file_types": &file_types,
                "result_types": &result_types,
                "max_results": max_results,
            }),
            json!({
                "query": &query,
                "path": different_path,
                "file_types": &file_types,
                "result_types": &result_types,
                "max_results": max_results,
            }),
            json!({
                "query": &query,
                "path": path.as_deref(),
                "file_types": changed_file_types,
                "result_types": &result_types,
                "max_results": max_results,
            }),
            json!({
                "query": &query,
                "path": path.as_deref(),
                "file_types": &file_types,
                "result_types": changed_result_types,
                "max_results": max_results,
            }),
            json!({
                "query": &query,
                "path": path.as_deref(),
                "file_types": &file_types,
                "result_types": &result_types,
                "max_results": different_max_results,
            }),
        ] {
            prop_assert!(
                !replays_code_search(cached_args.clone(), &changed_args),
                "each changed effective code-search dimension must execute fresh: {changed_args}"
            );
        }
    }
}

#[test]
fn code_search_replay_separates_different_effective_limits() {
    let history = ToolExecutionHistory::new(10);

    history.add_record(ToolExecutionRecord::success(
        tools::CODE_SEARCH.to_string(),
        tools::CODE_SEARCH.to_string(),
        false,
        None,
        json!({"query": "ToolRegistry", "max_results": 1}),
        json!({"results": ["limited search"]}),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let replayed = history.find_recent_successful_by_read_target(
        tools::CODE_SEARCH,
        &json!({"query": "ToolRegistry", "max_results": 100}),
        Duration::from_secs(60),
    );

    assert!(replayed.is_none());
}
