//! Covers runtime prompt section rendering and replacement.
use super::*;

#[test]
fn runtime_tool_prompt_sections_use_explicit_profile_for_active_tools() {
    let mut powershell_prompt = "Base prompt".to_string();
    let mut unix_prompt = "Base prompt".to_string();
    let snapshot = SessionToolCatalogueSnapshot::new(
        7,
        9,
        false,
        false,
        Some(std::sync::Arc::new(vec![
            ToolDefinition::function(
                TOOL_EXEC_COMMAND.to_string(),
                "Shell".to_string(),
                serde_json::json!({"type": "object"}),
            ),
            ToolDefinition::function(
                TOOL_CODE_SEARCH.to_string(),
                "Bounded source search".to_string(),
                serde_json::json!({"type": "object"}),
            ),
        ])),
        false,
    );

    append_runtime_tool_prompt_sections_for_profile(
        &mut powershell_prompt,
        &snapshot,
        false,
        ResolvedShellPromptProfile::PowerShell,
    );
    append_runtime_tool_prompt_sections_for_profile(
        &mut unix_prompt,
        &snapshot,
        false,
        ResolvedShellPromptProfile::UnixLike,
    );

    assert!(powershell_prompt.contains("## Active Tools"));
    assert!(powershell_prompt.contains("`Get-ChildItem`"));
    assert!(powershell_prompt.contains("`Select-String`"));
    assert!(!powershell_prompt.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));

    assert!(unix_prompt.contains("## Active Tools"));
    assert!(unix_prompt.contains("Browse: `exec_command.cmd`"));
    for command in ["ls", "rg", "find", "cat", "sed", "awk"] {
        assert!(unix_prompt.contains(&format!("`{command}`")), "Unix browse guidance should include {command}");
    }
    assert!(unix_prompt.contains("Advanced `code_search` takes `query`"));
    assert!(unix_prompt.contains("literal smart-case"));
    assert!(!unix_prompt.contains("`Get-ChildItem`"));
}

#[test]
fn runtime_tool_prompt_sections_include_catalogue_metadata() {
    let mut prompt = "Base prompt".to_string();
    let snapshot = SessionToolCatalogueSnapshot::new(
        7,
        9,
        true,
        false,
        Some(std::sync::Arc::new(vec![
            ToolDefinition::function(
                TOOL_EXEC_COMMAND.to_string(),
                "Search".to_string(),
                serde_json::json!({"type": "object"}),
            ),
            ToolDefinition::function(
                TOOL_APPLY_PATCH.to_string(),
                "File".to_string(),
                serde_json::json!({"type": "object"}),
            ),
        ])),
        false,
    );

    append_runtime_tool_prompt_sections(&mut prompt, &snapshot, true);

    assert!(prompt.contains("## Active Tools"));
    assert!(prompt.contains("[Runtime Tool Catalogue]"));
    assert!(prompt.contains("catalogue_tools: 2"));
    assert!(prompt.contains("currently_available_tools: exec_command, apply_patch"));
    assert!(prompt.contains("request_user_input_enabled: false"));
}

/// Render the catalogue metadata block for a single-tool snapshot.
fn render_catalogue_metadata(version: u64, epoch: u64) -> String {
    let mut prompt = "Base prompt".to_string();
    let snapshot = SessionToolCatalogueSnapshot::new(
        version,
        epoch,
        false,
        false,
        Some(std::sync::Arc::new(vec![ToolDefinition::function(
            TOOL_READ_FILE.to_string(),
            "Read a file".to_string(),
            serde_json::json!({"type": "object"}),
        )])),
        false,
    );

    append_runtime_tool_prompt_sections(&mut prompt, &snapshot, true);

    let metadata_start = prompt
        .find("[Runtime Tool Catalogue]")
        .expect("catalogue metadata should be appended");
    prompt[metadata_start..].trim_end().to_string()
}

/// Replace the per-session version and epoch so the metadata can be compared
/// as a stable snapshot.
fn normalise_catalogue_metadata(metadata: &str) -> String {
    metadata
        .lines()
        .map(|line| {
            if line.starts_with("- version: ") {
                "- version: <version>".to_string()
            } else if line.starts_with("- epoch: ") {
                "- epoch: <epoch>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn runtime_catalogue_metadata_snapshot_is_stable_across_variable_values() {
    let first = render_catalogue_metadata(1, 2);
    let second = render_catalogue_metadata(41, 99);

    assert_eq!(
        normalise_catalogue_metadata(&first),
        normalise_catalogue_metadata(&second),
        "only the version and epoch should vary between sessions"
    );

    let expected = "\
[Runtime Tool Catalogue]
- version: <version>
- epoch: <epoch>
- catalogue_tools: 1
- available_tools: 1
- currently_available_tools: read_file
- request_user_input_enabled: false";
    assert_eq!(normalise_catalogue_metadata(&first), expected);

    assert!(first.contains("- version: 1"));
    assert!(first.contains("- epoch: 2"));
    assert!(second.contains("- version: 41"));
    assert!(second.contains("- epoch: 99"));
}

#[test]
fn runtime_tool_prompt_sections_replace_existing_runtime_sections() {
    let mut prompt = "Base prompt".to_string();
    let first = SessionToolCatalogueSnapshot::new(
        1,
        2,
        false,
        false,
        Some(std::sync::Arc::new(vec![ToolDefinition::function(
            TOOL_EXEC_COMMAND.to_string(),
            "Search".to_string(),
            serde_json::json!({"type": "object"}),
        )])),
        false,
    );
    let second = SessionToolCatalogueSnapshot::new(
        7,
        9,
        true,
        true,
        Some(std::sync::Arc::new(vec![ToolDefinition::function(
            TOOL_APPLY_PATCH.to_string(),
            "File".to_string(),
            serde_json::json!({"type": "object"}),
        )])),
        false,
    );

    append_runtime_tool_prompt_sections(&mut prompt, &first, true);
    append_runtime_tool_prompt_sections(&mut prompt, &second, true);

    assert_eq!(prompt.matches("## Active Tools").count(), 1);
    assert_eq!(prompt.matches("[Runtime Tool Catalogue]").count(), 1);
    assert!(prompt.contains("version: 7"));
    assert!(!prompt.contains("version: 1"));
    assert!(prompt.contains("request_user_input_enabled: true"));
    assert!(!prompt.contains("request_user_input_enabled: false"));
}
