#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

use anyhow::Result;
use serde_json::json;
use tempfile::TempDir;
use vtcode_core::config::constants::tools;
use vtcode_core::tools::ToolRegistry;

#[tokio::test]
async fn preflight_normalizes_functions_code_search_namespace() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    let outcome = registry.preflight_validate_call(
        "functions.code_search",
        &json!({"query": "ReasoningStage", "path": ".", "max_results": 10}),
    )?;
    assert_eq!(outcome.normalized_tool_name, tools::CODE_SEARCH);
    assert!(outcome.readonly_classification);
    Ok(())
}

#[tokio::test]
async fn code_search_rejects_every_former_public_field() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    for field in [
        "action",
        "workflow",
        "pattern",
        "kind",
        "config_path",
        "filter",
        "lang",
        "selector",
        "strictness",
        "view",
        "items",
        "type",
        "match",
        "pub_members",
        "follow",
        "debug_query",
        "globs",
        "skip_snapshot_tests",
        "context_lines",
        "severities",
        "no_ignore",
        "threads",
        "format",
        "report_style",
        "before_lines",
        "after_lines",
        "builtin_rules",
    ] {
        let mut payload = json!({"query": "ReasoningStage"});
        let _previous = payload
            .as_object_mut()
            .expect("request object")
            .insert(field.to_string(), json!(true));
        let err = registry
            .preflight_validate_call(tools::CODE_SEARCH, &payload)
            .expect_err("former public field must fail schema validation");
        let text = err.to_string();
        assert!(text.contains("Invalid arguments for tool 'code_search'"));
        assert!(text.contains(field), "{field}: {text}");
    }
    Ok(())
}

#[tokio::test]
async fn code_search_validates_five_property_contract() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    let valid = registry.preflight_validate_call(
        tools::CODE_SEARCH,
        &json!({
            "query": "ReasoningStage",
            "path": "src",
            "file_types": ["rust", ".rs"],
            "result_types": ["usage", "definition"],
            "max_results": 5
        }),
    )?;
    assert_eq!(valid.normalized_tool_name, tools::CODE_SEARCH);
    assert!(valid.readonly_classification);

    for invalid in [
        json!({}),
        json!({"query": ""}),
        json!({"query": "   "}),
        json!({"query": 42}),
        json!({"query": "Widget", "path": 42}),
        json!({"query": "Widget", "file_types": []}),
        json!({"query": "Widget", "file_types": "rust"}),
        json!({"query": "Widget", "file_types": ["unknown-language"]}),
        json!({"query": "Widget", "result_types": []}),
        json!({"query": "Widget", "result_types": ["reference"]}),
        json!({"query": "Widget", "result_types": "text"}),
        json!({"query": "Widget", "max_results": 0}),
        json!({"query": "Widget", "max_results": 101}),
        // Note: `max_results: "20"` (string) is intentionally accepted via
        // schema-aware coercion — see `preflight_coerces_stringified_max_results`.
        json!({"query": "Widget", "max_results": "20.5"}),
        json!({"query": "Widget", "max_results": "not-a-number"}),
    ] {
        let error = registry
            .preflight_validate_call(tools::CODE_SEARCH, &invalid)
            .expect_err("invalid request must fail preflight");
        assert!(error.to_string().contains("Invalid arguments for tool 'code_search'"), "{invalid}: {error}");
    }
    Ok(())
}

/// A JSON-encoded string for an integer/array field is coerced to the typed
/// value so the model does not waste retries on a serialization mistake. This
/// is the compatibility counterpart to the strict rejections above: only
/// unambiguous JSON parses are accepted, and bounds/enum still apply.
#[tokio::test]
async fn preflight_coerces_stringified_max_results() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    let outcome =
        registry.preflight_validate_call(tools::CODE_SEARCH, &json!({"query": "Widget", "max_results": "20"}))?;
    assert_eq!(outcome.effective_args["max_results"], json!(20));

    // Bounds still enforced after coercion.
    let over_max = registry
        .preflight_validate_call(tools::CODE_SEARCH, &json!({"query": "Widget", "max_results": "101"}))
        .expect_err("coerced integer must still be bounds-checked");
    assert!(over_max.to_string().contains("Invalid arguments for tool 'code_search'"));
    Ok(())
}

#[tokio::test]
async fn code_search_registry_executor_returns_normalized_typed_response() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let registry = ToolRegistry::new(temp_dir.path().to_path_buf()).await;

    let response = registry
        .execute_tool(
            tools::CODE_SEARCH,
            json!({
                "query": "  Widget  ",
                "file_types": [".rs", "rust"],
                "result_types": ["path", "path"]
            }),
        )
        .await?;

    let mut top_level_fields = response
        .as_object()
        .expect("response object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    top_level_fields.sort_unstable();
    assert_eq!(top_level_fields, ["filters", "hints", "query", "results", "returned", "truncated"]);
    assert_eq!(response["query"], "Widget");
    assert_eq!(response["filters"]["path"], ".");
    assert_eq!(response["filters"]["file_types"], json!(["rust"]));
    assert_eq!(response["filters"]["result_types"], json!(["path"]));
    assert_eq!(response["filters"]["max_results"], 20);
    assert_eq!(response["results"], json!([]));
    assert_eq!(response["returned"], 0);
    assert_eq!(response["truncated"], false);
    assert_eq!(response["hints"], json!([]));
    Ok(())
}
