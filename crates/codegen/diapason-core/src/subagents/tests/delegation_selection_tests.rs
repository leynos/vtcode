//! Explicit delegation and model-selection request parsing.
use super::*;
use rstest::rstest;

#[test]
fn request_prompt_prefers_message() {
    let request = SpawnAgentRequest {
        message: Some("hello".to_string()),
        ..SpawnAgentRequest::default()
    };
    assert_eq!(request_prompt(&request.message, &request.items).as_deref(), Some("hello"));
}

#[test]
fn delegated_task_requires_clarification_for_vague_prompt() {
    assert!(delegated_task_requires_clarification("report"));
    assert!(delegated_task_requires_clarification("report findings"));
    assert!(!delegated_task_requires_clarification("review current code changes"));
}

#[test]
fn explicit_delegation_request_detects_mentions_and_keywords() {
    let direct_mentions = extract_explicit_agent_mentions("@agent-worker fix the issue", &[]);
    assert!(contains_explicit_delegation_request("@agent-worker fix the issue", direct_mentions.as_slice()));
    let no_mentions = extract_explicit_agent_mentions("delegate this in parallel", &[]);
    assert!(contains_explicit_delegation_request("delegate this in parallel", no_mentions.as_slice()));
    let empty_mentions = extract_explicit_agent_mentions("review the repository", &[]);
    assert!(!contains_explicit_delegation_request("review the repository", empty_mentions.as_slice()));
}

/// The three phrasings the natural-language selection accepts.
///
/// Each case names the agent exactly once and expects that mention to win, so a
/// regression in one phrasing is reported by its own case name.
#[rstest]
#[case::natural_language("rust-engineer", "use rust-engineer agent to review current code")]
#[case::looser_subagent("background-demo", "use background-demo and run the subagent")]
#[case::run_subagent("rust-engineer", "run rust-engineer subagent and review changes")]
fn explicit_agent_mentions_detect_natural_language_selection(#[case] agent_name: &str, #[case] prompt: &str) {
    let spec = read_only_test_spec(agent_name);
    assert_eq!(
        extract_explicit_agent_mentions(prompt, &[spec]),
        vec![agent_name.to_string()],
        "the mention in {prompt:?} must select {agent_name}"
    );
}

#[test]
fn explicit_agent_mentions_ignore_primary_only_agents() {
    let mut duck = read_only_test_spec("duck");
    duck.mode = diapason_config::AgentMode::Primary;

    assert_eq!(
        extract_explicit_agent_mentions("@agent-duck discuss the task", &[duck.clone()]),
        Vec::<String>::new()
    );
    assert_eq!(
        extract_explicit_agent_mentions("run duck agent and discuss the task", &[duck]),
        Vec::<String>::new()
    );
}

#[test]
fn explicit_model_request_detects_aliases_and_full_ids() {
    assert!(contains_explicit_model_request("delegate this using gpt-5.6-luna", "gpt-5.6-luna"));
    assert!(contains_explicit_model_request("use the worker subagent with haiku", "haiku"));
    assert!(contains_explicit_model_request("run this with the small model", "small"));
    assert!(!contains_explicit_model_request("delegate this small cleanup task", "small"));
    assert!(!contains_explicit_model_request("delegate this task", "gpt-5.6-luna"));
}

#[test]
fn normalize_requested_model_override_drops_default_like_values() {
    assert_eq!(normalize_requested_model_override(Some("default".to_string()), "delegate this task"), None);
    assert_eq!(normalize_requested_model_override(Some(" inherit ".to_string()), "delegate this task"), None);
    assert_eq!(
        normalize_requested_model_override(Some(" inherit ".to_string()), "delegate this task using inherit"),
        Some("inherit".to_string())
    );
}

#[test]
fn sanitize_subagent_input_items_drops_empty_fields() {
    let mut items = vec![
        SubagentInputItem {
            item_type: Some("text".to_string()),
            text: Some("  Workspace: /tmp/repo  ".to_string()),
            path: Some(String::new()),
            name: Some(" ".to_string()),
            image_url: None,
        },
        SubagentInputItem {
            item_type: Some("text".to_string()),
            text: Some("   ".to_string()),
            path: Some(String::new()),
            name: None,
            image_url: None,
        },
    ];

    sanitize_subagent_input_items(&mut items);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text.as_deref(), Some("Workspace: /tmp/repo"));
    assert!(items[0].path.is_none());
    assert!(items[0].name.is_none());
}
