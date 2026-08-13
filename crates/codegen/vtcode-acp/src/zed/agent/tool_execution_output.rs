use super::ZedAgent;
use crate::acp;
use serde_json::{Map, Value};
use vtcode_core::utils::ansi_parser::strip_ansi;

const SUMMARY_FIELDS: &[(&str, &str)] = &[
    ("command", "Command"),
    ("working_directory", "Working directory"),
    ("backend", "Backend"),
    ("session_id", "Session ID"),
    ("success", "Tool succeeded"),
    ("is_exited", "Process exited"),
    ("exit_code", "Exit code"),
    ("process_id", "Process ID"),
    ("wall_time", "Wall time (seconds)"),
];

struct ExecCommandPresentation {
    summary: String,
    output: String,
    details: Option<String>,
}

impl ExecCommandPresentation {
    fn from_output(output: &Value) -> Self {
        let Some(fields) = output.as_object() else {
            return Self {
                summary: "Command completed".to_string(),
                output: display_value(output),
                details: None,
            };
        };

        let summary = render_summary(fields);
        let command_output = fields.get("output").map_or_else(
            || "(no command output returned)".to_string(),
            |value| value.as_str().map_or_else(|| display_value(value), strip_ansi),
        );
        let details = render_additional_details(fields);

        Self { summary, output: command_output, details }
    }
}

impl ZedAgent {
    pub(super) fn render_exec_command_content(&self, output: &Value) -> Vec<acp::ToolCallContent> {
        let presentation = ExecCommandPresentation::from_output(output);
        let mut blocks = Vec::with_capacity(3);
        blocks.push(acp::ToolCallContent::from(presentation.summary));
        blocks.push(acp::ToolCallContent::from(self.render_command_output_block(&presentation.output)));
        if let Some(details) = presentation.details {
            blocks.push(acp::ToolCallContent::from(self.render_command_details_block(&details)));
        }
        blocks
    }

    fn render_command_output_block(&self, output: &str) -> String {
        let (rendered, truncated) = self.truncate_text(output);
        if truncated {
            format!(
                "Command output\n{rendered}\n[command output truncated for display; complete data remains in rawOutput]"
            )
        } else {
            format!("Command output\n{rendered}")
        }
    }

    fn render_command_details_block(&self, details: &str) -> String {
        let (rendered, truncated) = self.truncate_text(details);
        if truncated {
            format!(
                "Execution details\n{rendered}\n[execution details truncated for display; complete data remains in rawOutput]"
            )
        } else {
            format!("Execution details\n{rendered}")
        }
    }
}

fn render_summary(fields: &Map<String, Value>) -> String {
    let lines = SUMMARY_FIELDS
        .iter()
        .filter_map(|(key, label)| fields.get(*key).map(|value| format!("{label}: {}", display_value(value))))
        .collect::<Vec<_>>();

    if lines.is_empty() {
        "Command completed".to_string()
    } else {
        lines.join("\n")
    }
}

fn render_additional_details(fields: &Map<String, Value>) -> Option<String> {
    let lines = fields
        .iter()
        .filter(|(key, _)| {
            key.as_str() != "output" && !SUMMARY_FIELDS.iter().any(|(summary_key, _)| key == summary_key)
        })
        .map(|(key, value)| format!("{}: {}", field_label(key), display_value(value)))
        .collect::<Vec<_>>();

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn field_label(key: &str) -> String {
    let mut label = key.replace('_', " ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => strip_ansi(text),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exec_command_presentation_separates_output_from_execution_metadata() {
        let presentation = ExecCommandPresentation::from_output(&json!({
            "success": true,
            "output": "first line\nsecond line\n",
            "wall_time": 0.212,
            "session_id": "run-123",
            "command": "git status --short",
            "working_directory": "/workspace",
            "backend": "pipe",
            "is_exited": true,
            "exit_code": 0,
            "total_output_bytes": 23,
            "output_truncated": false,
            "spool_path": ".vtcode/context/tool_outputs/run-123.txt",
            "spool_complete": true
        }));

        assert_eq!(
            presentation.summary,
            "Command: git status --short\nWorking directory: /workspace\nBackend: pipe\nSession ID: run-123\nTool succeeded: true\nProcess exited: true\nExit code: 0\nWall time (seconds): 0.212"
        );
        assert_eq!(presentation.output, "first line\nsecond line\n");
        assert_eq!(
            presentation.details.as_deref(),
            Some(
                "Total output bytes: 23\nOutput truncated: false\nSpool path: .vtcode/context/tool_outputs/run-123.txt\nSpool complete: true"
            )
        );
    }

    #[test]
    fn exec_command_presentation_preserves_unknown_structured_details() {
        let presentation = ExecCommandPresentation::from_output(&json!({
            "output": "",
            "future_metadata": {
                "attempts": 2,
                "recovered": true
            },
            "next_action": "retry"
        }));

        let details = presentation.details.expect("additional details should be rendered");
        assert!(details.contains("Future metadata: {\n  \"attempts\": 2,\n  \"recovered\": true\n}"));
        assert!(details.contains("Next action: retry"));
    }

    #[test]
    fn exec_command_presentation_strips_terminal_escape_sequences() {
        let presentation = ExecCommandPresentation::from_output(&json!({
            "command": "printf colour",
            "output": "\u{1b}[31mred\u{1b}[0m"
        }));

        assert_eq!(presentation.summary, "Command: printf colour");
        assert_eq!(presentation.output, "red");
    }
}
