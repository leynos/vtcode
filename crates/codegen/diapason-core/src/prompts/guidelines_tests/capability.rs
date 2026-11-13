//! Covers capability detection and compact guidance budgets.
use super::*;

#[test]
fn test_capability_basic_guidance() {
    let tools = vec![];
    let guidelines = generate_tool_guidelines(&tools, Some(CapabilityLevel::Basic));
    assert!(guidelines.contains("Capabilities: limited"));
    assert!(guidelines.contains("enable more capabilities"));
}

#[test]
fn test_capability_file_reading_guidance() {
    let tools = vec![TOOL_APPLY_PATCH.to_string()];
    let guidelines = generate_tool_guidelines(&tools, Some(CapabilityLevel::FileReading));
    assert!(guidelines.contains("Capabilities: read-only"));
    assert!(guidelines.contains("do not modify"));
}

#[test]
fn test_full_capabilities_no_special_guidance() {
    let tools = vec![
        TOOL_APPLY_PATCH.to_string(),
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
    ];
    let guidelines = generate_tool_guidelines_for_profile(
        &tools,
        Some(CapabilityLevel::Editing),
        ResolvedShellPromptProfile::UnixLike,
    );

    assert!(!guidelines.contains("Capabilities: limited"));
    assert!(!guidelines.contains("Capabilities: read-only"));
}

#[test]
fn test_empty_tools_shows_read_only_capabilities() {
    let tools = vec![];
    let guidelines = generate_tool_guidelines(&tools, None);
    assert!(guidelines.contains("Capabilities: read-only"));
}

#[test]
fn test_planning_workflow_guidance_keeps_verification_open() {
    let tools = vec![
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_TASK_TRACKER.to_string(),
        TOOL_CODE_SEARCH.to_string(),
    ];
    let guidelines = generate_runtime_tool_guidelines_for_profile(&tools, true, ResolvedShellPromptProfile::UnixLike);
    assert!(guidelines.contains("Keep `task_tracker` updated"));
    assert!(guidelines.contains("blockers and verification open"));
}

#[test]
fn test_capability_inference_precedence() {
    let tools = vec![TOOL_APPLY_PATCH.to_string(), TOOL_CODE_SEARCH.to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::CodeSearch);

    let tools = vec![TOOL_EXEC_COMMAND.to_string(), TOOL_APPLY_PATCH.to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::Editing);
}

#[test]
fn test_capability_inference_variants() {
    let tools = vec![TOOL_APPLY_PATCH.to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::Editing);

    let tools = vec![TOOL_EXEC_COMMAND.to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::Bash);

    let tools = vec![TOOL_CODE_SEARCH.to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::CodeSearch);

    let tools = vec![TOOL_LIST_FILES.to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::FileListing);

    let tools = vec!["read_file".to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::FileReading);

    let tools = vec!["unknown_tool".to_string()];
    assert_eq!(infer_capability_level(&tools), CapabilityLevel::Basic);
}
