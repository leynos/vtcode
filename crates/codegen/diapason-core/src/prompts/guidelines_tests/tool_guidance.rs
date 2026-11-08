//! Covers tool preference, shell-profile, and browse guidance text.
use super::*;

#[test]
fn test_read_only_capability_detection() {
    let tools = vec![TOOL_CODE_SEARCH.to_string()];
    let guidelines = generate_tool_guidelines(&tools, None);
    assert!(guidelines.contains("Capabilities: read-only"));
    assert!(guidelines.contains("do not modify files"));
}

#[test]
fn test_tool_preference_guidance() {
    let tools = vec![TOOL_EXEC_COMMAND.to_string(), TOOL_CODE_SEARCH.to_string()];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);
    assert!(guidelines.contains("Advanced `code_search` takes `query`"));
    assert!(guidelines.contains("literal smart-case"));
    assert!(guidelines.contains("exact syntactic usages"));
    assert!(guidelines.contains("\"result_types\":[\"definition\"]"));
    assert!(guidelines.contains("Do not JSON-encode arrays or integers as strings"));
    assert!(guidelines.contains("omit unused filters"));
    assert!(guidelines.contains("path: \"\""));
    assert!(guidelines.contains("git diff -- <path>"));
    assert!(guidelines.contains("build tools"));
    assert!(guidelines.contains("test tools"));
    assert!(guidelines.contains("Completion is a checkpoint"));
}

#[test]
fn test_edit_workflow_guidance() {
    let tools = vec![TOOL_APPLY_PATCH.to_string()];
    let guidelines = generate_tool_guidelines(&tools, None);
    assert!(guidelines.contains("Use `apply_patch`"));
    assert!(guidelines.contains("patches small"));
    assert!(guidelines.contains("verification resolved"));
}

#[test]
fn test_diapason_guidance_omits_task_tracker() {
    let tools = vec![
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_WRITE_STDIN.to_string(),
        TOOL_APPLY_PATCH.to_string(),
    ];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

    assert!(guidelines.contains("exec_command.cmd"));
    for command in ["ls", "rg", "find", "cat", "sed", "awk"] {
        assert!(
            guidelines.contains(&format!("`{command}`")),
            "{command} should be shown as an exec_command.cmd example"
        );
    }
    assert!(guidelines.contains("`write_stdin`"));
    assert!(guidelines.contains("`apply_patch`"));
    assert!(!guidelines.contains("task_tracker"));
    assert!(!guidelines.contains("list_files"));
    assert!(!guidelines.contains("read_file"));
}

#[test]
fn task_tracker_guidance_explains_action_aware_indices() {
    let guidelines = generate_runtime_tool_guidelines_for_profile(
        &[TOOL_TASK_TRACKER.to_string()],
        true,
        ResolvedShellPromptProfile::UnixLike,
    );

    assert!(guidelines.contains("positive flat indices"));
    assert!(guidelines.contains("index: 0"));
    assert!(guidelines.contains("items"));
}

#[test]
fn unix_like_guidance_makes_command_reuse_explicit() {
    let tools = vec![TOOL_EXEC_COMMAND.to_string(), TOOL_WRITE_STDIN.to_string()];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

    assert!(guidelines.contains("One-shot `exec_command` calls"));
    assert!(guidelines.contains("`!!`, `!$`, `!ssh`, or `fc`"));
    assert!(guidelines.contains("write full command arguments explicitly"));
    assert!(guidelines.contains("conversation or tool results"));
    assert!(guidelines.contains("existing `session_id`"));
    assert!(guidelines.contains("Bash `histverify`"));
    assert!(guidelines.contains("zsh `HIST_VERIFY`"));
}

#[test]
fn powershell_guidance_uses_native_command_examples() {
    let tools = vec![
        TOOL_EXEC_COMMAND.to_string(),
        TOOL_CODE_SEARCH.to_string(),
        TOOL_APPLY_PATCH.to_string(),
    ];
    let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::PowerShell);

    assert!(guidelines.contains("native PowerShell commands"));
    assert!(guidelines.contains("`Get-ChildItem`"));
    assert!(guidelines.contains("`Select-String`"));
    assert!(guidelines.contains("native PowerShell syntax"));
    assert!(guidelines.contains("Advanced `code_search` takes `query`"));
    assert!(guidelines.contains("literal smart-case"));
    assert!(guidelines.contains("omit unused filters"));
    assert!(!guidelines.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
    assert!(!guidelines.contains("shell history expansion"));
    assert!(!guidelines.contains("histverify"));
    assert!(!guidelines.contains("HIST_VERIFY"));
}

#[test]
fn shell_profile_prompt_keeps_policy_and_syntax_separate() {
    let unix = render_shell_profile_guidance(ResolvedShellPromptProfile::UnixLike);
    assert!(unix.contains("Active shell profile: `unix_like`"));
    assert!(unix.contains("does not rewrite GNU flags for macOS BSD tools"));
    assert!(unix.contains("controls prompt examples and expected command syntax only"));
    assert!(unix.contains("does not translate GNU-to-BSD"));

    let powershell = render_shell_profile_guidance(ResolvedShellPromptProfile::PowerShell);
    assert!(powershell.contains("Active shell profile: `powershell`"));
    assert!(powershell.contains("WSL"));
    assert!(powershell.contains("Unix-like workflows"));
    assert!(powershell.contains("PowerShell-to-Unix"));
}

#[test]
fn test_harness_browse_tool_guidance() {
    let tools = vec![TOOL_LIST_FILES.to_string(), TOOL_READ_FILE.to_string()];
    let guidelines = generate_tool_guidelines(&tools, None);
    assert!(guidelines.contains("available read-only repository tools"));
    assert!(guidelines.contains("bound `read_file` ranges"));
    assert!(!guidelines.contains("list_files"));
    assert!(!guidelines.contains("offset"));
    assert!(!guidelines.contains("per_page"));
}

#[test]
fn test_canonical_browse_tool_guidance_prefers_public_tools() {
    let tools = vec![
        TOOL_CODE_SEARCH.to_string(),
        TOOL_LIST_FILES.to_string(),
        "read_file".to_string(),
    ];
    let guidelines = generate_tool_guidelines(&tools, None);
    assert!(guidelines.contains("available read-only repository tools"));
    assert!(guidelines.contains("code_search"));
    assert!(guidelines.contains("bound `read_file` ranges"));
}
