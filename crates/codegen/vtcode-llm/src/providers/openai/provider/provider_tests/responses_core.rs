//! Core Responses API payload construction tests.

use super::*;

// ─── Responses API Payload Tests ─────────────────────────────────────────────

#[test]
fn responses_payload_uses_function_wrapper() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let payload = responses_payload_for(models::openai::GPT_5, &provider);
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    let tool = tools[0].as_object().expect("tool entry should be object");
    assert_eq!(tool.get("type").and_then(Value::as_str), Some("function"));
    assert_eq!(tool.get("name").and_then(Value::as_str), Some("search_workspace"));
    assert!(tool.contains_key("parameters"));
}

#[test]
fn responses_payload_omits_default_verbosity_for_gpt_5_2_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_2_CODEX);
    let payload = responses_payload_for(models::openai::GPT_5_2_CODEX, &provider);
    assert_absent(&payload, "text");
}

#[test]
fn responses_payload_ignores_configured_verbosity_for_gpt_5_2_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_2_CODEX);
    let mut request = sample_request(models::openai::GPT_5_2_CODEX);
    request.verbosity = Some(vtcode_config::types::VerbosityLevel::Medium);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "text");
}

#[test]
fn responses_payload_omits_default_verbosity_for_gpt_5_3_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let payload = responses_payload_for(models::openai::GPT_5_CODEX, &provider);
    assert_absent(&payload, "text");
}

#[test]
fn responses_payload_keeps_configured_verbosity_for_gpt_5_3_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.verbosity = Some(vtcode_config::types::VerbosityLevel::High);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_eq!(payload.get("text").and_then(|t| t.get("verbosity")).and_then(Value::as_str), Some("high"));
}

#[test]
fn responses_payload_keeps_configured_verbosity_for_gpt_5_4() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.verbosity = Some(vtcode_config::types::VerbosityLevel::High);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_eq!(payload.get("text").and_then(|t| t.get("verbosity")).and_then(Value::as_str), Some("high"));
}

#[test]
fn responses_payload_passes_context_management() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.context_management = Some(json!([{"type": "compaction", "compact_threshold": 200000}]));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let mgmt = payload
        .get("context_management")
        .and_then(Value::as_array)
        .expect("context_management should be present");
    assert_eq!(mgmt.len(), 1);
    assert_eq!(mgmt[0].get("type").and_then(Value::as_str), Some("compaction"));
}

#[test]
fn responses_payload_sets_instructions_from_system_prompt() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_str_field(&payload, "instructions", "You are a helpful assistant.");
    let input = get_input_array(&payload);
    assert_eq!(input.first().and_then(|v| v.get("role")).and_then(Value::as_str), Some("user"));
}

#[test]
fn responses_payload_applies_gpt_5_6_addendum_only_for_gpt_5_6() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .expect("instructions should exist");
    assert!(instructions.contains("You are a helpful assistant."));
    assert!(instructions.contains("## GPT-5.6 OpenAI Addendum"));

    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .expect("instructions should exist");
    assert!(!instructions.contains("## GPT-5.6 OpenAI Addendum"));
}

#[test]
fn responses_payload_applies_gpt_5_6_addendum_with_priority_service_tier() {
    assert!(OpenAIProvider::is_responses_api_model(models::openai::GPT_5_6_SOL));

    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_5_6_SOL.to_string()),
        None,
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");

    assert_eq!(payload.get("service_tier").and_then(Value::as_str), Some("priority"));
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .expect("instructions should exist");
    assert!(instructions.contains("## GPT-5.6 OpenAI Addendum"));
}

#[test]
fn responses_payload_omits_previous_response_and_includes_optional_fields() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.previous_response_id = Some("resp_previous_123".to_string());
    request.response_store = Some(false);
    request.responses_include = Some(vec![
        "reasoning.encrypted_content".to_string(),
        "output_text.annotations".to_string(),
    ]);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "previous_response_id");
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    let include = payload
        .get("include")
        .and_then(Value::as_array)
        .expect("include should be present");
    assert_eq!(include.len(), 2);
    assert_eq!(include.first().and_then(Value::as_str), Some("reasoning.encrypted_content"));
}

#[test]
fn compatible_responses_payload_omits_previous_response_id() {
    let provider = compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1");
    let mut request = sample_request(models::openai::GPT_5);
    request.previous_response_id = Some("resp_previous_123".to_string());
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "previous_response_id");
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
}

#[test]
fn responses_payload_serializes_user_input_file_by_id() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::text("Summarize this file".to_string()),
            provider::ContentPart::file_from_id("file-abc123".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let input = get_input_array(&payload);
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("user content should be an array");
    assert!(content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("input_file")
            && part.get("file_id").and_then(Value::as_str) == Some("file-abc123")
    }));
}

#[test]
fn responses_payload_serializes_user_input_file_data() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::text("Summarize this file".to_string()),
            provider::ContentPart::file_from_data("report.pdf".to_string(), "aGVsbG8=".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let input = get_input_array(&payload);
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("user content should be an array");
    assert!(content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("input_file")
            && part.get("filename").and_then(Value::as_str) == Some("report.pdf")
            && part.get("file_data").and_then(Value::as_str) == Some("aGVsbG8=")
    }));
}

// ─── Hosted Shell Tests ──────────────────────────────────────────────────────

#[test]
fn responses_payload_uses_hosted_shell_when_enabled() {
    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(hosted_shell_openai_config()),
        None,
    );
    let request = shell_request(models::openai::GPT_5);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tool = payload["tools"][0].as_object().expect("tool entry should be object");
    assert_str_field_obj(tool, "type", "shell");
    assert_eq!(tool["environment"]["type"].as_str(), Some("container_auto"));
    assert_eq!(tool["environment"]["network_policy"]["type"].as_str(), Some("disabled"));
    assert_eq!(tool["environment"]["file_ids"][0].as_str(), Some("file_123"));
    assert_eq!(tool["environment"]["skills"][0]["type"].as_str(), Some("skill_reference"));
    assert!(tool["environment"]["skills"][0].get("version").is_none());
    let output_types = payload["output_types"].as_array().expect("output types should be present");
    assert!(output_types.iter().any(|v| v.as_str() == Some("shell_call")));
}

#[test]
fn responses_payload_serializes_hosted_shell_allowlist_and_domain_secrets() {
    let provider = OpenAIProvider::from_config(
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
                    allowed_domains: vec!["httpbin.org".to_string()],
                    domain_secrets: vec![OpenAIHostedShellDomainSecret {
                        domain: "httpbin.org".to_string(),
                        name: "API_KEY".to_string(),
                        value: "debug-secret-123".to_string(),
                    }],
                },
            },
            ..Default::default()
        }),
        None,
    );
    let request = shell_request(models::openai::GPT_5);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    let np = &payload["tools"][0]["environment"]["network_policy"];
    assert_eq!(np["type"].as_str(), Some("allowlist"));
    assert_eq!(np["allowed_domains"][0].as_str(), Some("httpbin.org"));
    assert_eq!(np["domain_secrets"][0]["domain"].as_str(), Some("httpbin.org"));
    assert_eq!(np["domain_secrets"][0]["name"].as_str(), Some("API_KEY"));
    assert_eq!(np["domain_secrets"][0]["value"].as_str(), Some("debug-secret-123"));
}

#[test]
fn responses_payload_omits_explicit_latest_version_and_uses_container_reference() {
    let provider = OpenAIProvider::from_config(
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
                    skill_id: "skill_123".to_string(),
                    version: OpenAIHostedSkillVersion::String(" latest ".to_string()),
                }],
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let request = shell_request(models::openai::GPT_5);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert!(payload["tools"][0]["environment"]["skills"][0].get("version").is_none());

    // Container reference should use container_id, omit file_ids/skills
    let provider2 = OpenAIProvider::from_config(
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
                container_id: Some("cntr_123".to_string()),
                file_ids: vec!["file_ignored".to_string()],
                skills: vec![OpenAIHostedSkill::SkillReference {
                    skill_id: "skill_ignored".to_string(),
                    version: OpenAIHostedSkillVersion::default(),
                }],
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let request2 = shell_request(models::openai::GPT_5);
    let payload2 = provider2.convert_to_openai_responses_format(&request2).expect("should succeed");
    let env = &payload2["tools"][0]["environment"];
    assert_eq!(env["type"].as_str(), Some("container_reference"));
    assert_eq!(env["container_id"].as_str(), Some("cntr_123"));
    assert!(env.get("file_ids").is_none() && env.get("skills").is_none());
}
