#![allow(
    missing_docs,
    dead_code,
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

//! Passive JSON schemas for utility, file, scheduling, and collaboration tool surfaces.

#![recursion_limit = "256"]
use serde_json::{Value, json};

mod collaboration;
mod json_schema;
#[cfg(feature = "mcp")]
mod mcp_tool;
mod responses_api;
mod tool_kind;

pub use collaboration::{
    agent_parameters, close_agent_parameters, request_user_input_description, request_user_input_parameters,
    resume_agent_parameters, send_input_parameters, spawn_agent_parameters, spawn_background_subprocess_parameters,
    wait_agent_parameters,
};
pub use json_schema::{AdditionalProperties, JsonSchema, parse_tool_input_schema};
#[cfg(feature = "mcp")]
pub use mcp_tool::{ParsedMcpTool, parse_mcp_tool};
pub use responses_api::{FreeformTool, FreeformToolFormat, ResponsesApiTool};
pub(crate) use tool_kind::{CanonicalToolMeta, TokenBucket, ToolKind, ToolNamespace};

pub const SEMANTIC_ANCHOR_GUIDANCE: &str =
    "Prefer stable semantic @@ anchors such as function, class, method, or impl names.";

/// Explicit, format-bearing description for the `patch` alias field. The old
/// value ("Alias for input") gave the model no format guidance, so it often
/// placed a standard unified diff (`---`/`+++`) there — which `apply_patch`
/// rejects. This mirrors the `input` description so both alias fields carry
/// identical, complete format guidance (see checkpoint turn_615 for the
/// failure this prevents).
pub const APPLY_PATCH_ALIAS_DESCRIPTION: &str = "Patch in VT Code format (*** Begin Patch, *** Update File: path, @@ hunk, -/+ lines, *** End Patch). Same envelope as 'input'; do NOT use unified diff (--- /+++ format). Every patch path must be workspace-relative; never use absolute paths, `..`, or traversal-like forms.";
pub const DEFAULT_APPLY_PATCH_INPUT_DESCRIPTION: &str = "Patch in VT Code format: *** Begin Patch, *** Update File: path, @@ hunk, -/+ lines, *** End Patch. Every patch path must be workspace-relative; never use absolute paths, `..`, or traversal-like forms.";

/// Default model-visible preview budget for function-tool results.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
/// Smallest valid model-visible preview budget for a function-tool result.
pub const MIN_MAX_OUTPUT_TOKENS: usize = 1;
/// Largest valid model-visible preview budget for a function-tool result.
pub const MAX_MAX_OUTPUT_TOKENS: usize = 50_000;
/// Canonical JSON property name for the common result-preview budget field.
///
/// Centralized so the strict dedicated validator in `vtcode-core` and any
/// compatibility coercion both reference one name and cannot drift.
pub const MAX_OUTPUT_TOKENS_FIELD: &str = "max_output_tokens";

/// Adds the common result-preview budget field to an object-shaped tool schema.
///
/// The returned schema deliberately preserves every existing constraint,
/// including `additionalProperties`, so callers can use it for legacy schemas
/// without weakening their argument validation.
#[must_use]
pub fn with_max_output_tokens_parameter(mut schema: Value) -> Value {
    let Some(schema_object) = schema.as_object_mut() else {
        return schema;
    };
    let properties = schema_object
        .entry("properties")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(properties_object) = properties.as_object_mut() else {
        return schema;
    };
    {
        let _entry = properties_object.entry(MAX_OUTPUT_TOKENS_FIELD).or_insert_with(|| {
            json!({
                "type": "integer",
                "minimum": MIN_MAX_OUTPUT_TOKENS,
                "maximum": MAX_MAX_OUTPUT_TOKENS,
                "default": DEFAULT_MAX_OUTPUT_TOKENS,
                "description": "Maximum number of result tokens returned to the model. Full oversized output is spooled when available."
            })
        });
    }
    schema
}

#[must_use]
pub fn with_semantic_anchor_guidance(base: &str) -> String {
    let trimmed = base.trim_end();
    if trimmed.contains(SEMANTIC_ANCHOR_GUIDANCE) {
        trimmed.to_string()
    } else if trimmed.ends_with('.') {
        format!("{trimmed} {SEMANTIC_ANCHOR_GUIDANCE}")
    } else {
        format!("{trimmed}. {SEMANTIC_ANCHOR_GUIDANCE}")
    }
}

#[must_use]
pub fn apply_patch_parameter_schema(input_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": with_semantic_anchor_guidance(input_description)
            },
            "patch": {
                "type": "string",
                "description": with_semantic_anchor_guidance(APPLY_PATCH_ALIAS_DESCRIPTION)
            }
        },
        "anyOf": [
            {"required": ["input"]},
            {"required": ["patch"]}
        ]
    })
}

#[must_use]
pub fn apply_patch_parameters() -> Value {
    apply_patch_parameter_schema(DEFAULT_APPLY_PATCH_INPUT_DESCRIPTION)
}

#[must_use]
pub fn cron_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["action"],
        "additionalProperties": false,
        "properties": {
            "action": {
                "type": "string",
                "enum": ["create", "list", "delete"],
                "description": "create: schedule a prompt (requires prompt and exactly one of cron, delay_minutes, or run_at). list: show scheduled prompts. delete: remove one by id."
            },
            "prompt": {"type": "string", "description": "create: prompt to run when the task fires."},
            "name": {"type": "string", "description": "create: optional short label for the task."},
            "cron": {"type": "string", "description": "create: five-field cron expression for recurring tasks."},
            "delay_minutes": {"type": "integer", "description": "create: fixed recurring interval in minutes."},
            "run_at": {"type": "string", "description": "create: one-shot fire time in RFC3339 or local datetime form."},
            "id": {"type": "string", "description": "delete: session scheduled task id to delete."}
        }
    })
}

#[must_use]
pub fn mcp_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["search_tools", "get_tool_details", "list_servers", "connect", "disconnect"],
                "description": "search_tools: find MCP tools by natural-language query. get_tool_details: fetch the full input schema for one MCP tool name. list_servers: list configured servers and their connection state. connect or disconnect: manage one configured MCP server by name."
            },
            "query": {"type": "string", "description": "search_tools: natural language query describing the MCP capability to find."},
            "detail_level": {"type": "string", "enum": ["name", "name_description", "full"], "description": "search_tools: response detail level."},
            "limit": {"type": "integer", "minimum": 1, "maximum": 25, "description": "search_tools: maximum number of results to return."},
            "name": {"type": "string", "description": "get_tool_details: exact MCP tool name. connect or disconnect: configured MCP server name."}
        },
        "additionalProperties": false
    })
}

#[must_use]
pub fn cron_create_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["prompt"],
        "additionalProperties": false,
        "properties": {
            "prompt": {"type": "string", "description": "Prompt to run when the task fires."},
            "name": {"type": "string", "description": "Optional short label for the task."},
            "cron": {"type": "string", "description": "Five-field cron expression for recurring tasks."},
            "delay_minutes": {"type": "integer", "description": "Fixed recurring interval in minutes."},
            "run_at": {
                "type": "string",
                "description": "One-shot fire time in RFC3339 or local datetime form. Use this instead of `cron` or `delay_minutes` for reminders."
            }
        }
    })
}

#[must_use]
pub fn cron_list_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

#[must_use]
pub fn cron_delete_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {"type": "string", "description": "Session scheduled task id to delete."}
        }
    })
}

#[must_use]
pub fn exec_command_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["cmd"],
        "properties": {
            "cmd": {"type": "string", "description": "Shell command to execute, subject to command policy. Examples include `ls`, `rg`, `find`, `cat`, `sed`, `awk`, build tools, and test tools."},
            "yield_time_ms": {"type": "integer", "description": "Wait before returning output (ms). If the command is still running, the response includes a session_id for write_stdin.", "default": 10000},
            "max_output_tokens": {"type": "integer", "minimum": 1, "maximum": 50000, "default": 10000, "description": "Output token cap. Large or truncated output can return a spool_path; an active session may set spool_complete=false for a readable partial snapshot, while an exited pending spool is withheld until a later wait."},
            "workdir": {"type": "string", "description": "Working directory."},
            "tty": {"type": "boolean", "description": "Run the command in PTY mode for interactive or terminal-sensitive commands.", "default": false},
            "sandbox_permissions": {
                "type": "string",
                "enum": ["use_default", "with_additional_permissions", "require_escalated", "bypass_sandbox"],
                "description": "Sandbox permission mode for this command. Omit it or use `use_default` for the normal sandbox. Non-empty `additional_permissions` is normalized to `with_additional_permissions`. `require_escalated` and `bypass_sandbox` require a non-empty `justification` and cannot be combined with `additional_permissions`.",
                "default": "use_default"
            },
            "additional_permissions": {
                "type": "object",
                "description": "Optional extra filesystem roots to grant inside the sandbox. Non-empty `additional_permissions` implicitly requests `with_additional_permissions`; every path is normalized and must stay within allowed workspace or temp roots.",
                "properties": {
                    "fs_read": {"type": "array", "items": {"type": "string"}},
                    "fs_write": {"type": "array", "items": {"type": "string"}}
                },
                "additionalProperties": false
            },
            "justification": {"type": "string", "description": "Short approval question for expanded sandbox scope. Required when sandbox_permissions is `require_escalated` or `bypass_sandbox`."}
        },
        "additionalProperties": false
    })
}

#[must_use]
pub fn write_stdin_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["session_id"],
        "properties": {
            "session_id": {"type": "string", "description": "Active execution session id."},
            "action": {"type": "string", "enum": ["write", "poll", "wait"], "description": "Use write with chars to send input, poll to fetch fresh output without input, or wait to block until the command exits or wait_timeout_seconds expires; wait never kills the session."},
            "chars": {"type": "string", "description": "Bytes to write to stdin. Pass an empty string to poll without sending input."},
            "yield_time_ms": {"type": "integer", "description": "Wait before returning fresh session output (ms).", "default": 1000},
            "wait_timeout_seconds": {"type": "integer", "minimum": 1, "description": "Explicit wait deadline in seconds. A deadline returns an in-progress session that can be waited on again."},
            "timeout_seconds": {"type": "integer", "minimum": 1, "description": "Alias for wait_timeout_seconds."},
            "max_output_tokens": {"type": "integer", "minimum": 1, "maximum": 50000, "default": 10000, "description": "Output token cap for the continuation response. Large or truncated output can return a spool_path; the response reports whether an active session has finished writing it."}
        },
        "anyOf": [
            {"required": ["chars"]},
            {"required": ["action"], "properties": {"action": {"const": "poll"}}},
            {"required": ["action"], "properties": {"action": {"const": "wait"}}}
        ],
        "additionalProperties": false
    })
}

#[must_use]
pub fn search_tools_parameters() -> Value {
    with_max_output_tokens_parameter(json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "description": "Natural-language description of the capability to discover."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 25,
                "default": 5
            },
            "detail_level": {
                "type": "string",
                "enum": ["name", "name_description", "full"],
                "default": "name_description"
            }
        },
        "additionalProperties": false
    }))
}

#[must_use]
pub fn code_search_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "additionalProperties": false,
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "pattern": "\\S",
                "description": "Literal code or path query. Smart-case applies to content and exact symbol-name matching: a wholly lower-case query matches case-insensitively, while an upper-case character makes matching case-sensitive. Path matching remains fuzzy and case-insensitive."
            },
            "path": {
                "type": "string",
                "minLength": 1,
                "pattern": "\\S",
                "description": "Workspace-relative file or directory to search. Omit to search the workspace root."
            },
            "file_types": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "pattern": "\\S"
                },
                "description": "Language names or common file extensions, with or without one leading dot."
            },
            "result_types": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "string",
                    "enum": ["definition", "usage", "text", "path"]
                },
                "description": "Result categories to include. Omit to include all four categories."
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 100,
                "description": "Maximum number of merged results to return. Omit for 20."
            }
        }
    })
}

#[must_use]
pub fn list_files_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Directory or file path to inspect.", "default": "."},
            "mode": {
                "type": "string",
                "enum": ["list", "recursive", "tree", "find_name", "find_content", "largest", "file", "files"],
                "description": "Listing mode. Use page/per_page to continue paginated results.",
                "default": "list"
            },
            "pattern": {"type": "string", "description": "Optional glob-style path filter."},
            "name_pattern": {"type": "string", "description": "Optional name filter for list/find_name modes."},
            "content_pattern": {"type": "string", "description": "Content query for find_content mode."},
            "page": {"type": "integer", "description": "1-indexed results page.", "minimum": 1},
            "per_page": {"type": "integer", "description": "Items per page.", "minimum": 1},
            "max_results": {"type": "integer", "description": "Maximum total results to consider before pagination.", "minimum": 1},
            "include_hidden": {"type": "boolean", "description": "Include dotfiles and hidden entries.", "default": false},
            "response_format": {"type": "string", "enum": ["concise", "detailed"], "description": "Verbosity of the listing output.", "default": "concise"},
            "case_sensitive": {"type": "boolean", "description": "Case-sensitive name matching.", "default": false}
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn apply_patch_parameter_schema_keeps_alias_and_guidance_consistent() {
        let schema = apply_patch_parameter_schema("Patch in VT Code format");

        // Both `input` and `patch` alias fields now carry the format
        // description AND the semantic-anchor guidance, preventing the model
        // from placing a unified diff in `patch` (see checkpoint turn_615).
        assert_eq!(
            schema["properties"]["patch"]["description"],
            with_semantic_anchor_guidance(APPLY_PATCH_ALIAS_DESCRIPTION)
        );
        let patch_description = schema["properties"]["patch"]["description"]
            .as_str()
            .expect("patch description");
        assert!(patch_description.contains("*** Begin Patch"));
        assert!(patch_description.contains("unified diff"));
        assert!(patch_description.contains(SEMANTIC_ANCHOR_GUIDANCE));

        let input_description = schema["properties"]["input"]["description"]
            .as_str()
            .expect("input description");
        assert!(input_description.contains(SEMANTIC_ANCHOR_GUIDANCE));
    }

    #[test]
    fn common_output_limit_parameter_preserves_strict_schema() {
        let schema = with_max_output_tokens_parameter(json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "additionalProperties": false
        }));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["max_output_tokens"]["default"], json!(DEFAULT_MAX_OUTPUT_TOKENS));
        assert_eq!(schema["properties"]["max_output_tokens"]["minimum"], json!(MIN_MAX_OUTPUT_TOKENS));
        assert_eq!(schema["properties"]["max_output_tokens"]["maximum"], json!(MAX_MAX_OUTPUT_TOKENS));
    }

    #[test]
    fn codex_baseline_exec_schemas_use_public_names_shape() {
        let exec_params = exec_command_parameters();
        assert_eq!(exec_params["required"], json!(["cmd"]));
        assert!(exec_params["properties"]["cmd"].is_object());
        assert!(exec_params["properties"]["workdir"].is_object());
        assert!(
            exec_params["properties"]["yield_time_ms"]["description"]
                .as_str()
                .expect("exec yield description")
                .contains("session_id")
        );
        assert!(
            exec_params["properties"]["max_output_tokens"]["description"]
                .as_str()
                .expect("exec max output description")
                .contains("spool_path")
        );
        assert_eq!(exec_params["properties"]["tty"]["type"], "boolean");
        assert_eq!(exec_params["properties"]["tty"]["default"], false);
        assert_eq!(exec_params["properties"]["yield_time_ms"]["default"], 10000);
        assert_eq!(
            exec_params["properties"]["sandbox_permissions"]["enum"],
            json!([
                "use_default",
                "with_additional_permissions",
                "require_escalated",
                "bypass_sandbox"
            ])
        );
        assert_eq!(exec_params["properties"]["sandbox_permissions"]["default"], "use_default");
        assert_eq!(
            exec_params["properties"]["additional_permissions"]["properties"]["fs_read"]["items"]["type"],
            "string"
        );
        assert_eq!(
            exec_params["properties"]["additional_permissions"]["properties"]["fs_write"]["items"]["type"],
            "string"
        );
        assert_eq!(exec_params["properties"]["additional_permissions"]["additionalProperties"], false);
        assert_eq!(exec_params["properties"]["justification"]["type"], "string");
        assert!(
            exec_params["properties"]["sandbox_permissions"]["description"]
                .as_str()
                .expect("sandbox_permissions description")
                .contains("normalized to `with_additional_permissions`")
        );
        assert!(
            exec_params["properties"]["additional_permissions"]["description"]
                .as_str()
                .expect("additional_permissions description")
                .contains("allowed workspace or temp roots")
        );
        assert!(
            exec_params["properties"]["justification"]["description"]
                .as_str()
                .expect("justification description")
                .contains("Required when sandbox_permissions is `require_escalated` or `bypass_sandbox`")
        );
        assert_eq!(exec_params["additionalProperties"], false);
        for command in ["ls", "rg", "find", "cat", "sed", "awk"] {
            assert!(
                exec_params["properties"]["cmd"]["description"]
                    .as_str()
                    .expect("cmd description")
                    .contains(command),
                "{command} should be described as an exec_command.cmd example"
            );
            assert!(
                exec_params["properties"].get(command).is_none(),
                "{command} must not be modelled as a separate exec_command field"
            );
        }

        let stdin_params = write_stdin_parameters();
        assert_eq!(stdin_params["required"], json!(["session_id"]));
        assert!(stdin_params["properties"]["session_id"].is_object());
        assert_eq!(stdin_params["properties"]["chars"]["type"], "string");
        assert_eq!(stdin_params["properties"]["action"]["enum"], json!(["write", "poll", "wait"]));
        assert!(stdin_params["properties"]["wait_timeout_seconds"].is_object());
        assert_eq!(stdin_params["anyOf"][1]["required"], json!(["action"]));
        assert_eq!(stdin_params["anyOf"][1]["properties"]["action"]["const"], "poll");
        assert_eq!(stdin_params["anyOf"][2]["required"], json!(["action"]));
        assert_eq!(stdin_params["anyOf"][2]["properties"]["action"]["const"], "wait");
        assert!(
            stdin_params["properties"]["chars"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("empty string"))
        );
        assert!(stdin_params["properties"]["chars"].is_object());
        assert!(
            stdin_params["properties"]["yield_time_ms"]["description"]
                .as_str()
                .expect("stdin yield description")
                .contains("fresh session output")
        );
        assert!(
            stdin_params["properties"]["max_output_tokens"]["description"]
                .as_str()
                .expect("stdin max output description")
                .contains("spool_path")
        );
        assert_eq!(stdin_params["additionalProperties"], false);
    }

    #[test]
    fn code_search_schema_exposes_exact_five_property_contract() {
        let params = code_search_parameters();
        let properties = params["properties"].as_object().expect("properties");
        let mut property_names = properties.keys().map(String::as_str).collect::<Vec<_>>();
        property_names.sort_unstable();

        assert_eq!(params["required"], json!(["query"]));
        assert_eq!(property_names, ["file_types", "max_results", "path", "query", "result_types"]);
        assert_eq!(params["additionalProperties"], false);
        assert_eq!(params["properties"]["query"]["pattern"], "\\S");
        assert_eq!(params["properties"]["file_types"]["minItems"], 1);
        assert_eq!(params["properties"]["result_types"]["minItems"], 1);
        assert_eq!(
            params["properties"]["result_types"]["items"]["enum"],
            json!(["definition", "usage", "text", "path"])
        );
        assert_eq!(params["properties"]["max_results"]["minimum"], 1);
        assert_eq!(params["properties"]["max_results"]["maximum"], 100);
        assert!(params.get("anyOf").is_none());
    }

    #[test]
    fn legacy_list_files_schema_exposes_pagination_fields() {
        let list_params = list_files_parameters();
        assert!(list_params["properties"]["page"].is_object());
        assert!(list_params["properties"]["per_page"].is_object());
        assert!(
            list_params["properties"]["mode"]["enum"]
                .as_array()
                .expect("mode enum")
                .iter()
                .any(|value| value == "recursive")
        );
    }

    #[test]
    fn semantic_anchor_guidance_is_appended_once() {
        let base = "Patch in VT Code format.";
        let with_guidance = with_semantic_anchor_guidance(base);

        assert!(with_guidance.contains(SEMANTIC_ANCHOR_GUIDANCE));
        assert_eq!(with_semantic_anchor_guidance(&with_guidance), with_guidance);
    }

    #[test]
    fn default_apply_patch_parameters_keep_expected_alias_shape() {
        let schema = apply_patch_parameters();

        assert_eq!(
            schema["anyOf"],
            json!([
                {"required": ["input"]},
                {"required": ["patch"]}
            ])
        );
    }
}
