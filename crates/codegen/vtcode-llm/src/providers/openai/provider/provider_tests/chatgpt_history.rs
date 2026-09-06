//! ChatGPT backend structured-history preservation tests.

use super::*;

// ─── ChatGPT Backend History Tests ───────────────────────────────────────────

#[test]
fn chatgpt_backend_omits_previous_response_id_from_responses_payload() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.previous_response_id = Some("resp_previous_123".to_string());
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "previous_response_id");
}

// Helper to build ChatGPT backend history payload
fn chatgpt_codex_payload(messages: Vec<provider::Message>, model: &str) -> Value {
    let provider = chatgpt_backend_provider(model);
    let request = provider::LLMRequest {
        messages: messages.into(),
        model: model.to_string(),
        ..Default::default()
    };
    provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed")
}

#[test]
fn chatgpt_backend_keeps_plain_assistant_history_structured_for_codex() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("What is this project?".to_owned()),
            provider::Message::assistant("VT Code is a Rust Cargo workspace.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("Tell me more.".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 3);
    assert_eq!(input_role_at(&payload, 0), Some("user"));
    assert_eq!(input_role_at(&payload, 1), Some("assistant"));
    assert_absent(&input[1], "phase");
    assert_eq!(input_role_at(&payload, 2), Some("user"));
    assert!(
        payload["instructions"]
            .as_str()
            .unwrap()
            .contains("You are Codex, based on GPT-5.")
    );
}

#[test]
fn chatgpt_backend_preserves_reasoning_detail_items_for_codex_follow_up() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::assistant("Hello. What would you like me to do?".to_owned())
                .with_reasoning_details(Some(vec![json!({
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{"type":"summary_text","text":"task prompt"}]
                })]))
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("tell me more".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 3);
    assert_eq!(input_type_at(&payload, 0), Some("reasoning"));
    assert_eq!(input_role_at(&payload, 1), Some("assistant"));
    assert_absent(&input[1], "phase");
    assert_eq!(input_role_at(&payload, 2), Some("user"));
}

#[test]
fn chatgpt_backend_keeps_tool_turn_history_structured_for_codex() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("run cargo check".to_owned()),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "call_1".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"cargo check\"}".to_string(),
                )],
            ),
            provider::Message::tool_response(
                "call_1".to_string(),
                "{\"output\":\"Finished `dev` profile\",\"exit_code\":0}".to_string(),
            ),
            provider::Message::assistant("cargo check completed successfully.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("who are you".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 5);
    assert_eq!(input_role_at(&payload, 0), Some("user"));
    assert_eq!(input_type_at(&payload, 1), Some("function_call"));
    assert_eq!(input_call_id_at(&payload, 1), Some("call_1"));
    assert_eq!(input_type_at(&payload, 2), Some("function_call_output"));
    assert_eq!(input_call_id_at(&payload, 2), Some("call_1"));
    assert_eq!(input_role_at(&payload, 3), Some("assistant"));
    assert_eq!(input_role_at(&payload, 4), Some("user"));
    assert_absent(&input[3], "phase");
}

// Parametrized phase omission tests for ChatGPT backend models
#[test]
fn chatgpt_backend_omits_assistant_phase_for_codex_models() {
    for model in [models::openai::GPT_5_CODEX, models::openai::GPT_5_6_SOL] {
        let payload = chatgpt_codex_payload(
            vec![
                provider::Message::user("Run the next check.".to_owned()),
                provider::Message::assistant("Checking.".to_owned())
                    .with_phase(Some(provider::AssistantPhase::Commentary)),
                provider::Message::assistant("Done.".to_owned())
                    .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
                provider::Message::user("Continue.".to_owned()),
            ],
            model,
        );
        let input = get_input_array(&payload);
        assert_eq!(input.len(), 4);
        assert_absent(&input[1], "phase");
        assert_absent(&input[2], "phase");
    }
}

#[test]
fn chatgpt_backend_preserves_structured_tool_turns_with_paired_function_calls() {
    for model in [models::openai::GPT_5_CODEX, models::openai::GPT_5_6_SOL] {
        let payload = chatgpt_codex_payload(
            vec![
                provider::Message::user("Investigate the failing check.".to_owned()),
                provider::Message::assistant_with_tools(
                    "Checking the first command output.".to_owned(),
                    vec![provider::ToolCall::function(
                        "call_1".to_string(),
                        "exec_command".to_string(),
                        "{\"command\":\"cargo check -p vtcode-core\"}".to_string(),
                    )],
                )
                .with_phase(Some(provider::AssistantPhase::Commentary)),
                provider::Message::tool_response(
                    "call_1".to_string(),
                    "{\"output\":\"warning: example\",\"exit_code\":0}".to_string(),
                ),
                provider::Message::assistant("Need one more inspection step.".to_owned())
                    .with_phase(Some(provider::AssistantPhase::Commentary)),
                provider::Message::user("Continue.".to_owned()),
            ],
            model,
        );
        let input = get_input_array(&payload);
        assert_eq!(input.len(), 6);
        assert_eq!(input_role_at(&payload, 0), Some("user"));
        assert_eq!(input_role_at(&payload, 1), Some("assistant"));
        assert_absent(&input[1], "phase");
        assert_eq!(input_type_at(&payload, 2), Some("function_call"));
        assert_eq!(input_call_id_at(&payload, 2), Some("call_1"));
        assert_eq!(input_type_at(&payload, 3), Some("function_call_output"));
        assert_eq!(input_call_id_at(&payload, 3), Some("call_1"));
        assert_eq!(input_role_at(&payload, 4), Some("assistant"));
        assert_absent(&input[4], "phase");
        assert_eq!(input_role_at(&payload, 5), Some("user"));
    }
}

#[test]
fn chatgpt_backend_replays_prior_direct_tool_turns() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("run cargo fmt".to_owned()),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "direct_exec_command_1".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"cargo fmt\"}".to_string(),
                )],
            ),
            provider::Message::tool_response(
                "direct_exec_command_1".to_string(),
                "{\"output\":\"\",\"exit_code\":0,\"backend\":\"pipe\"}".to_string(),
            ),
            provider::Message::assistant("cargo fmt completed successfully.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("continue".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 5);
    assert_eq!(input_type_at(&payload, 1), Some("function_call"));
    assert_eq!(input_type_at(&payload, 2), Some("function_call_output"));
    assert_eq!(input_call_id_at(&payload, 1), Some("direct_exec_command_1"));
    assert_eq!(input_call_id_at(&payload, 2), Some("direct_exec_command_1"));
    assert!(input.iter().all(|item| {
        let t = item.get("type").and_then(Value::as_str);
        t != Some("tool_call") && t != Some("tool_result")
    }));
}

#[test]
fn chatgpt_backend_synthesizes_missing_function_call_outputs_for_orphan_calls() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("Run commands".to_owned()),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "call_orphan".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"echo orphan\"}".to_string(),
                )],
            ),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "call_paired".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"echo paired\"}".to_string(),
                )],
            ),
            provider::Message::tool_response(
                "call_paired".to_string(),
                "{\"output\":\"paired\",\"exit_code\":0}".to_string(),
            ),
            provider::Message::user("continue".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert!(input.iter().any(|i| {
        i.get("type").and_then(Value::as_str) == Some("function_call")
            && i.get("call_id").and_then(Value::as_str) == Some("call_orphan")
    }));
    assert!(input.iter().any(|i| {
        i.get("type").and_then(Value::as_str) == Some("function_call_output")
            && i.get("call_id").and_then(Value::as_str) == Some("call_orphan")
            && i.get("output").and_then(Value::as_str) == Some("aborted")
    }));
    assert!(input.iter().any(|i| {
        i.get("type").and_then(Value::as_str) == Some("function_call_output")
            && i.get("call_id").and_then(Value::as_str) == Some("call_paired")
    }));
}
