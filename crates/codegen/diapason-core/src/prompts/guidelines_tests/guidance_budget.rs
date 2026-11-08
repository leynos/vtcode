//! Covers planning, batching, and runtime guidance budgets.
use super::*;

#[test]
fn test_guidelines_stay_compact() {
    let tools = vec![
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
        "read_file".to_string(),
        TOOL_LIST_FILES.to_string(),
        "apply_patch".to_string(),
    ];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);
    assert!(guidelines.contains("Batch independent read-only calls"));
    assert!(guidelines.contains("code_search"));
    let approx_tokens = guidelines.len() / 4;
    // The batching and bounded-diff guardrails are intentionally part of
    // the compact shared prompt; keep the budget below 400 tokens.
    assert!(approx_tokens < 400, "got ~{approx_tokens} tokens");
}

#[test]
fn test_parallel_tool_call_guidance() {
    let tools = vec![
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
        TOOL_APPLY_PATCH.to_string(),
    ];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);
    assert!(guidelines.contains("parallel"), "Should include parallel tool call guidance");
    assert!(guidelines.contains("inputs do not depend"), "Should mention independent inputs");
}

#[test]
fn test_read_only_batching_guidance_is_explicit() {
    let tools = vec![
        TOOL_CODE_SEARCH.to_string(),
        TOOL_READ_FILE.to_string(),
        TOOL_LIST_FILES.to_string(),
    ];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

    assert!(guidelines.contains("Batch independent read-only calls"));
    assert!(guidelines.contains("`read_file` ranges"));
    assert!(guidelines.contains("serialize mutations"));
}

#[test]
fn execution_agents_can_suggest_planning_for_demanding_tasks() {
    let tools = vec![TOOL_START_PLANNING.to_string(), TOOL_EXEC_COMMAND.to_string()];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

    assert!(guidelines.contains("call `start_planning`"));
    assert!(guidelines.contains("do not use it for straightforward changes"));
}

#[test]
fn planning_workflow_runtime_guidance_keeps_exec_read_only() {
    let tools = vec![
        TOOL_APPLY_PATCH.to_string(),
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
    ];
    let guidelines = generate_runtime_tool_guidelines_for_profile(&tools, true, ResolvedShellPromptProfile::UnixLike);

    assert!(guidelines.contains("Planning workflow active"));
    assert!(guidelines.contains("`exec_command` only for read-only verification"));
    assert!(guidelines.contains("concrete repository target"));
    assert!(guidelines.contains("emit only one `<proposed_plan>` block"));
    assert!(guidelines.contains("omit unused filters"));
    assert!(!guidelines.contains("Inspect before edit"));
}

#[test]
fn runtime_tool_guidance_uses_explicit_powershell_profile() {
    let tools = vec![
        TOOL_APPLY_PATCH.to_string(),
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
    ];
    let guidelines =
        generate_runtime_tool_guidelines_for_profile(&tools, false, ResolvedShellPromptProfile::PowerShell);

    assert!(guidelines.contains("native PowerShell commands"));
    assert!(guidelines.contains("`Get-ChildItem`"));
    assert!(guidelines.contains("`Select-String`"));
    assert!(guidelines.contains("native PowerShell syntax"));
    assert!(!guidelines.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
}

#[test]
fn runtime_tool_guidance_uses_explicit_unix_like_profile() {
    let tools = vec![
        TOOL_APPLY_PATCH.to_string(),
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
    ];
    let guidelines = generate_runtime_tool_guidelines_for_profile(&tools, false, ResolvedShellPromptProfile::UnixLike);

    assert!(guidelines.contains("Browse: `exec_command.cmd`"));
    for command in ["ls", "rg", "find", "cat", "sed", "awk"] {
        assert!(guidelines.contains(&format!("`{command}`")), "Unix browse guidance should include {command}");
    }
    assert!(guidelines.contains("Advanced `code_search` takes `query`"));
    assert!(guidelines.contains("literal smart-case"));
    assert!(guidelines.contains("shell tasks"));
    assert!(!guidelines.contains("native PowerShell commands"));
    assert!(!guidelines.contains("`Get-ChildItem`"));
}
