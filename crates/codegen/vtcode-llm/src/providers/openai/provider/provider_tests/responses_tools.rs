//! Responses API hosted-tool and input-validation tests.

use super::*;

// Hosted shell fallback: keeps local shell tool when conditions aren't met
#[test]
fn hosted_shell_keeps_local_tool_when_conditions_not_met() {
    // Non-native URL
    let p1 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://example.com/v1".to_string()),
        None,
        None,
        None,
        Some(hosted_shell_openai_config()),
        None,
    );
    let payload1 = p1
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    let t1 = payload1["tools"][0].as_object().expect("tool");
    assert_str_field_obj(t1, "type", "function");
    assert_str_field_obj(t1, "name", "shell");

    // Blank container reference
    let p2 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerReference,
                container_id: Some("   ".to_string()),
                file_ids: Vec::new(),
                skills: Vec::new(),
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let payload2 = p2
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    let t2 = payload2["tools"][0].as_object().expect("tool");
    assert_str_field_obj(t2, "type", "function");
    assert_str_field_obj(t2, "name", "shell");

    // Blank skill ID
    let p3 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerAuto,
                container_id: None,
                file_ids: Vec::new(),
                skills: vec![OpenAIHostedSkill::SkillReference {
                    skill_id: "   ".to_string(),
                    version: OpenAIHostedSkillVersion::default(),
                }],
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let payload3 = p3
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    assert_str_field_obj(payload3["tools"][0].as_object().expect("tool"), "type", "function");

    // Empty allowlist
    let p4 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerAuto,
                container_id: None,
                file_ids: Vec::new(),
                skills: Vec::new(),
                network_policy: OpenAIHostedShellNetworkPolicy {
                    policy_type: OpenAIHostedShellNetworkPolicyType::Allowlist,
                    allowed_domains: Vec::new(),
                    domain_secrets: Vec::new(),
                },
            },
            ..Default::default()
        }),
        None,
    );
    let payload4 = p4
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    assert_str_field_obj(payload4["tools"][0].as_object().expect("tool"), "type", "function");
}

// ─── Validation & Schema Tests ───────────────────────────────────────────────
#[test]
fn responses_validation_rejects_single_inline_file_over_limit() {
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::file_from_data("report.pdf".to_string(), "aGVsbG8=".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let err = OpenAIProvider::validate_inline_file_inputs_with_limit(&request, 4)
        .expect_err("inline file should exceed limit");
    match err {
        provider::LLMError::InvalidRequest { message, .. } => {
            assert!(message.contains("50 MB request limit"));
            assert!(message.contains("report.pdf"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn responses_validation_rejects_combined_inline_files_over_limit() {
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::file_from_data("a.txt".to_string(), "YWJj".to_string()),
            provider::ContentPart::file_from_data("b.txt".to_string(), "ZGVm".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let err = OpenAIProvider::validate_inline_file_inputs_with_limit(&request, 5)
        .expect_err("combined inline files should exceed limit");
    match err {
        provider::LLMError::InvalidRequest { message, .. } => {
            assert!(message.contains("50 MB request limit"));
            assert!(message.contains("total inline file bytes = 6"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

// NOTE: responses_function_tools_sanitize_openai_incompatible_parameter_keywords and
// responses_function_tools_strip_openai_schema_combinators_from_builtin_tools were removed
// because they required vtcode-core::tools types which are not available in vtcode-llm.
// If this test coverage is needed, move them to vtcode-core.

#[test]
fn responses_function_tools_add_empty_properties_for_bare_object_schema() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Hello".to_owned())].into(),
        tools: Some(Arc::new(vec![provider::ToolDefinition::function(
            "vtcode-clippy".to_owned(),
            "Run clippy on the workspace".to_owned(),
            json!({"type": "object", "additionalProperties": true}),
        )])),
        model: models::openai::GPT_5_CODEX.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let params = payload["tools"]
        .as_array()
        .expect("tool array")
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("vtcode-clippy"))
        .and_then(|t| t.get("parameters"))
        .expect("vtcode-clippy parameters");
    assert_eq!(params["type"].as_str(), Some("object"));
    assert_eq!(params["properties"], json!({}));
    assert_eq!(params["additionalProperties"], json!({"type": "string"}));
}

// ─── Specialized Tool Types ──────────────────────────────────────────────────
#[test]
fn responses_payload_serializes_hosted_web_search_tool() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Find the latest VT Code news".to_owned())].into(),
        tools: Some(Arc::new(vec![provider::ToolDefinition::web_search(
            json!({"search_context_size": "medium"}),
        )])),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].get("type").and_then(Value::as_str), Some("web_search"));
    assert_eq!(tools[0].get("search_context_size").and_then(Value::as_str), Some("medium"));
}

#[test]
fn responses_payload_serializes_file_search_tool() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Search the docs vector store".to_owned())].into(),
        tools: Some(Arc::new(vec![provider::ToolDefinition::file_search(
            json!({"vector_store_ids": ["vs_docs"]}),
        )])),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].get("type").and_then(Value::as_str), Some("file_search"));
    assert_eq!(
        tools[0]
            .get("vector_store_ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str),
        Some("vs_docs")
    );
}

#[test]
fn responses_payload_keeps_distinct_remote_mcp_tools() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Use both MCP servers".to_owned())].into(),
        tools: Some(Arc::new(vec![
            provider::ToolDefinition::mcp(json!({
                "server_label": "dmcp",
                "server_url": "https://dmcp-server.deno.dev/sse",
                "require_approval": "never"
            })),
            provider::ToolDefinition::mcp(json!({
                "server_label": "docs",
                "server_url": "https://docs.example/sse",
                "require_approval": "never"
            })),
        ])),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    assert_eq!(tools.len(), 2);
    assert!(
        tools
            .iter()
            .any(|t| t.get("server_label").and_then(Value::as_str) == Some("dmcp"))
    );
    assert!(
        tools
            .iter()
            .any(|t| t.get("server_label").and_then(Value::as_str) == Some("docs"))
    );
}
