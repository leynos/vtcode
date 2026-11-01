//! Tool specifications and command recognition for `apply_patch`.
//!
//! The description and grammar constants are large prose blocks; keeping them
//! beside the handler in `apply_patch_handler` would push that module past the
//! repository's 400-line limit, so the spec builders live here and are
//! re-exported from the parent module.

use serde_json::json;

use super::super::tool_handler::{FreeformTool, FreeformToolFormat, ResponsesApiTool, ToolSpec};
use crate::config::constants::tools;

/// Create freeform apply_patch tool spec (for GPT-5 style models)
pub fn create_apply_patch_freeform_tool() -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: tools::APPLY_PATCH.to_string(),
        description: APPLY_PATCH_DESCRIPTION.to_string(),
        format: FreeformToolFormat {
            lark_grammar: Some(APPLY_PATCH_LARK_GRAMMAR.to_string()),
            examples: vec![
                APPLY_PATCH_ADD_EXAMPLE.to_string(),
                APPLY_PATCH_UPDATE_EXAMPLE.to_string(),
            ],
        },
    })
}

/// Create JSON function apply_patch tool spec (for standard function calling)
pub fn create_apply_patch_json_tool() -> ToolSpec {
    use crate::tools::apply_patch::{
        APPLY_PATCH_ALIAS_DESCRIPTION, DEFAULT_APPLY_PATCH_INPUT_DESCRIPTION, with_semantic_anchor_guidance,
    };
    ToolSpec::Function(ResponsesApiTool {
        name: tools::APPLY_PATCH.to_string(),
        description: format!("{APPLY_PATCH_DESCRIPTION}\n\n{APPLY_PATCH_GRAMMAR_HELP}"),
        strict: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": with_semantic_anchor_guidance(DEFAULT_APPLY_PATCH_INPUT_DESCRIPTION)
                },
                "patch": {
                    "type": "string",
                    "description": with_semantic_anchor_guidance(APPLY_PATCH_ALIAS_DESCRIPTION)
                }
            },
            "required": ["input"],
            "additionalProperties": false
        }),
    })
}

/// Parse a shell command to check if it's an apply_patch invocation
pub(crate) fn parse_apply_patch_command(command: &[String]) -> (bool, Option<String>) {
    const APPLY_PATCH_COMMANDS: &[&str] = &["apply_patch", "applypatch"];

    match command {
        // Direct invocation: apply_patch <patch>
        [cmd, body] if APPLY_PATCH_COMMANDS.contains(&cmd.as_str()) => (true, Some(body.clone())),
        // Shell heredoc form is not directly supported here
        // The Codex implementation uses tree-sitter to parse these
        _ => (false, None),
    }
}

// Constants for tool descriptions
const APPLY_PATCH_DESCRIPTION: &str = r#"Use the `apply_patch` tool to edit files.
Your patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply. Every patch path must be workspace-relative; never use absolute paths, `..`, or traversal-like forms.

You can think of it as a high-level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename)."#;

const APPLY_PATCH_GRAMMAR_HELP: &str = r#"May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more "hunks", each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

- ` ` (space) for context lines
- `-` for lines to remove
- `+` for lines to add

Important rules:
- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
- File references must be workspace-relative; never use absolute paths, `..`, or traversal-like forms
- Prefer small hunks with stable semantic @@ anchors like function, class, method, or impl names"#;

const APPLY_PATCH_LARK_GRAMMAR: &str = r#"
patch := "*** Begin Patch" NEWLINE { operation } "*** End Patch"
operation := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" text NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE
"#;

const APPLY_PATCH_ADD_EXAMPLE: &str = r#"*** Begin Patch
*** Add File: hello.txt
+Hello world
*** End Patch"#;

const APPLY_PATCH_UPDATE_EXAMPLE: &str = r#"*** Begin Patch
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** End Patch"#;
