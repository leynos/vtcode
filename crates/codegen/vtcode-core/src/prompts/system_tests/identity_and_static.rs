//! Static profile, agent-identity, and token-estimation regressions.

use super::*;

#[test]
fn test_static_prompts_have_no_placeholders() {
    let _minimal = generate_minimal_instruction();
    let _lightweight = generate_lightweight_instruction();
    let _specialized = generate_specialized_instruction();

    let minimal_text = minimal_instruction_text();
    let lightweight_text = lightweight_instruction_text();
    let specialized_text = specialized_instruction_text();

    assert!(!minimal_text.contains("__UNIFIED_TOOL_GUIDANCE__"), "Minimal prompt has uninterpolated placeholder");
    assert!(
        !lightweight_text.contains("__UNIFIED_TOOL_GUIDANCE__"),
        "Lightweight prompt has uninterpolated placeholder"
    );
    assert!(
        !specialized_text.contains("__UNIFIED_TOOL_GUIDANCE__"),
        "Specialized prompt has uninterpolated placeholder"
    );
    assert!(
        !default_system_prompt().contains("__UNIFIED_TOOL_GUIDANCE__"),
        "Default prompt has uninterpolated placeholder"
    );
}

#[test]
fn test_agent_identity_labels() {
    // Test known agent names
    assert_eq!(agent_identity_label("build"), "VT Code (Build mode)");
    assert_eq!(agent_identity_label("auto"), "VT Code (Auto mode)");
    assert_eq!(agent_identity_label("duck"), "VT Code (Duck mode)");
    assert_eq!(agent_identity_label("plan"), "VT Code (Plan mode)");
    assert_eq!(agent_identity_label("explorer"), "VT Code (Explorer mode)");
    assert_eq!(agent_identity_label("worker"), "VT Code (Worker mode)");

    // Test unknown agent names
    assert_eq!(agent_identity_label("unknown"), "VT Code (unknown)");
    assert_eq!(agent_identity_label("custom"), "VT Code (custom)");
}

#[test]
fn test_apply_agent_identity() {
    let prompt = "# VT Code\n\nVT Code. Be concise and safe.\n\n## Contract\n- Rule 1";
    let result = apply_agent_identity(prompt, "VT Code (Build mode)");
    assert_eq!(
        result,
        "# VT Code (Build mode)\n\nVT Code (Build mode). Be concise and safe.\n\n## Contract\n- Rule 1"
    );
}

#[tokio::test]
async fn test_system_prompt_includes_agent_identity() {
    let mut config = VTCodeConfig {
        default_primary_agent: "build".to_string(),
        ..Default::default()
    };
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.starts_with("# VT Code (Build mode)"), "Should start with agent identity: {}", &result[..50]);
    assert!(
        result.contains("VT Code (Build mode). Be concise and safe."),
        "Should include agent identity in intro"
    );
}

#[tokio::test]
async fn test_system_prompt_auto_agent_identity() {
    let mut config = VTCodeConfig {
        default_primary_agent: "auto".to_string(),
        ..Default::default()
    };
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.starts_with("# VT Code (Auto mode)"), "Should start with auto agent identity");
}

#[tokio::test]
async fn test_system_prompt_duck_agent_identity() {
    let mut config = VTCodeConfig {
        default_primary_agent: "duck".to_string(),
        ..Default::default()
    };
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.starts_with("# VT Code (Duck mode)"), "Should start with duck agent identity");
}

#[test]
fn test_estimate_token_count() {
    assert_eq!(estimate_token_count(""), 0);
    assert_eq!(estimate_token_count("hello"), 2); // 5 chars / 4 = 1.25 -> ceil = 2
    assert_eq!(estimate_token_count("1234"), 1); // 4 chars / 4 = 1
    assert_eq!(estimate_token_count("12345"), 2); // 5 chars / 4 = 1.25 -> ceil = 2

    // Realistic prompt size check — these are estimates, not exact token counts
    let minimal_tokens = estimate_token_count(minimal_system_prompt());
    let default_tokens = estimate_token_count(default_system_prompt());
    assert!(minimal_tokens < 400, "Minimal prompt tokens: {minimal_tokens}");
    assert!(default_tokens < 700, "Default prompt tokens: {default_tokens}");
}
