//! Privacy-preserving summaries for JSONL agent traces.

use std::{
    fs::File,
    io::{BufRead, BufReader, Cursor},
    path::Path,
};

use anyhow::{Context, Result};
use serde_json::Value;

mod metrics;
mod model;
mod stream;

use metrics::{LatencyAccumulator, LifecycleTiming, UsageAccounting};
pub use model::{HarnessTraceSummary, LatencyStatistics, TokenUsage};

const GENERIC_ERROR_CATEGORY: &str = "error";
const MAX_DISTINCT_TOOL_LABELS: usize = 256;
const MAX_TRACE_LINE_BYTES: usize = 1_048_576;
const OTHER_TOOL_LABEL: &str = "other_tool";

/// Analyze JSONL text while retaining only aggregate, non-sensitive facts.
pub fn analyse_jsonl(input: &str) -> Result<HarnessTraceSummary> {
    analyse_jsonl_reader(Cursor::new(input.as_bytes()))
}

/// Analyze a buffered JSONL source without loading the complete trace into memory.
pub fn analyse_jsonl_reader<R: BufRead>(reader: R) -> Result<HarnessTraceSummary> {
    stream::analyse_jsonl_reader(reader)
}

/// Analyze a JSONL trace file and add path context to filesystem errors.
pub fn analyse_jsonl_file(path: impl AsRef<Path>) -> Result<HarnessTraceSummary> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("read trace file {}", path.display()))?;
    analyse_jsonl_reader(BufReader::new(file)).with_context(|| format!("analyse trace file {}", path.display()))
}

fn record_value(
    value: &Value,
    summary: &mut HarnessTraceSummary,
    latencies: &mut LatencyAccumulator,
    timing: &mut LifecycleTiming,
    usage: &mut UsageAccounting,
) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let payload = object
        .get("data")
        .and_then(Value::as_object)
        .or_else(|| object.get("event").and_then(Value::as_object))
        .unwrap_or(object);
    let event_type =
        string_field(object, &["type", "event", "kind"]).or_else(|| string_field(payload, &["type", "event", "kind"]));
    let is_thread_event = event_type.is_some_and(is_known_event_type);

    let mut recognized = is_thread_event;
    let mut error_recorded = false;
    timing.record(event_type, object, payload, latencies);
    if matches!(event_type, Some("turn.started" | "turn/start")) {
        summary.turns = summary.turns.saturating_add(1);
    }
    if matches!(event_type, Some("thread.completed"))
        && let Some(num_turns) = number_field_from(object, payload, &["num_turns"])
    {
        summary.turns = summary.turns.max(num_turns);
    }
    if matches!(event_type, Some("error" | "turn.failed")) {
        add_error(summary, error_category(payload, object).unwrap_or(GENERIC_ERROR_CATEGORY));
        recognized = true;
        error_recorded = true;
    }

    if matches!(event_type, Some("tool/result")) {
        if let Some(bytes) = tool_result_output_bytes(payload) {
            summary.output_bytes = summary.output_bytes.saturating_add(bytes);
        }
        if let Some(category) = tool_result_error_category(payload) {
            let category = canonical_error_category(category);
            add_error(
                summary,
                if category == GENERIC_ERROR_CATEGORY {
                    "tool_error"
                } else {
                    category
                },
            );
            error_recorded = true;
        }
        recognized = true;
    }

    if let Some(item) = payload.get("item").and_then(Value::as_object) {
        if matches!(event_type, Some("item.completed")) {
            recognized |= record_item(item, summary);
            if let Some(bytes) = output_bytes(item) {
                summary.output_bytes = summary.output_bytes.saturating_add(bytes);
            }
        } else {
            recognized = true;
        }
    }

    let deepseek_tool = string_field_from(payload, object, &["tool", "tool_name", "name"])
        .or_else(|| {
            payload
                .get("function")
                .and_then(Value::as_object)
                .and_then(|f| string_field(f, &["name"]))
        })
        .or_else(|| {
            object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|f| string_field(f, &["name"]))
        });
    let has_step = payload.contains_key("step")
        || payload.contains_key("step_id")
        || object.contains_key("step")
        || object.contains_key("step_id");
    let is_step_start = matches!(event_type, Some("step/start"));
    let is_synthetic_record = event_type.is_none();
    let is_tool_call = is_synthetic_record || matches!(event_type, Some("tool" | "tool_call" | "tool/call"));
    if is_step_start || (is_synthetic_record && (deepseek_tool.is_some() || has_step)) {
        recognized = true;
        if is_step_start || has_step {
            summary.steps = summary.steps.saturating_add(1);
        }
    }
    if is_tool_call && let Some(tool) = deepseek_tool {
        recognized = true;
        record_tool(summary, tool);
    }

    if let Some(latency) = number_field_from(object, payload, &["latency_ms", "duration_ms", "latency"]) {
        latencies.record(latency);
        recognized = true;
    }
    if is_tool_call && let Some(bytes) = output_bytes(payload).or_else(|| output_bytes(object)) {
        summary.output_bytes = summary.output_bytes.saturating_add(bytes);
        recognized = true;
    }
    if let Some(category) = error_category(payload, object) {
        if !error_recorded {
            add_error(summary, category);
        }
        recognized = true;
    }
    usage.record(event_type, payload);
    recognized || has_usage(payload)
}

fn record_item(item: &serde_json::Map<String, Value>, summary: &mut HarnessTraceSummary) -> bool {
    let Some(details_type) = item.get("type").and_then(Value::as_str) else {
        return false;
    };
    match details_type {
        "tool_invocation" | "mcp_tool_call" => {
            summary.steps = summary.steps.saturating_add(1);
            if let Some(tool) = string_field(item, &["tool_name", "name"]) {
                record_tool(summary, tool);
            }
            if string_field(item, &["status", "outcome"])
                .is_some_and(|status| status != "completed" && status != "success")
            {
                add_error(summary, string_field(item, &["outcome", "status"]).unwrap_or("tool_error"));
            }
            true
        }
        "command_execution" => {
            summary.steps = summary.steps.saturating_add(1);
            record_tool(summary, "command_execution");
            if item
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "failed")
            {
                add_error(summary, "command_failed");
            }
            true
        }
        "tool_output" => true,
        "harness" => {
            if let Some(event) = item.get("event").and_then(Value::as_str)
                && event.ends_with("failed")
            {
                add_error(summary, item.get("error_category").and_then(Value::as_str).unwrap_or(event));
            }
            true
        }
        "error" => {
            add_error(summary, "error");
            true
        }
        _ => false,
    }
}

fn is_known_event_type(event: &str) -> bool {
    event.starts_with("thread.")
        || event.starts_with("turn.")
        || event.starts_with("item.")
        || event.starts_with("turn/")
        || event.starts_with("step/")
        || event.starts_with("agent/")
        || event.starts_with("agent-preset/")
        || event.starts_with("approval/")
        || event.starts_with("assistant/")
        || event.starts_with("command/")
        || event.starts_with("goal/")
        || event.starts_with("permission/")
        || event.starts_with("request/")
        || event.starts_with("sandbox/")
        || event.starts_with("session/")
        || event.starts_with("todo/")
        || event.starts_with("user/")
        || event.starts_with("web/")
        || matches!(
            event,
            "error"
                | "context.reset"
                | "permission.requested"
                | "permission.resolved"
                | "tool/call"
                | "tool/result"
                | "assistant/message"
                | "reasoning-chunks"
                | "session"
                | "text-chunks"
                | "tool-call-chunks"
        )
}

fn string_field_from<'a>(
    primary: &'a serde_json::Map<String, Value>,
    fallback: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    string_field(primary, names).or_else(|| string_field(fallback, names))
}

fn number_field_from(
    primary: &serde_json::Map<String, Value>,
    fallback: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<u64> {
    number_field(primary, names).or_else(|| number_field(fallback, names))
}

fn tool_result_output_bytes(object: &serde_json::Map<String, Value>) -> Option<u64> {
    let content = object
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)?;
    let mut total = 0_u64;
    let mut found = false;
    for block in content {
        let Some(block) = block.as_object() else {
            continue;
        };
        let Some(fragments) = block.get("content") else {
            continue;
        };
        match fragments {
            Value::String(text) => {
                total = total.saturating_add(text.len() as u64);
                found = true;
            }
            Value::Array(fragments) => {
                for fragment in fragments {
                    if let Some(text) = fragment.as_str().or_else(|| {
                        fragment
                            .as_object()
                            .and_then(|fragment| fragment.get("text"))
                            .and_then(Value::as_str)
                    }) {
                        total = total.saturating_add(text.len() as u64);
                        found = true;
                    }
                }
            }
            _ => {}
        }
    }
    found.then_some(total)
}

fn tool_result_error_category(object: &serde_json::Map<String, Value>) -> Option<&str> {
    let content = object
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)?;
    for block in content {
        let Some(block) = block.as_object() else {
            continue;
        };
        if !block.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if let Some(category) = block.get("content").and_then(Value::as_array).and_then(|fragments| {
            fragments.iter().find_map(|fragment| {
                fragment
                    .as_object()
                    .and_then(|fragment| fragment.get("text"))
                    .and_then(Value::as_str)
                    .or_else(|| fragment.as_str())
            })
        }) {
            return Some(category);
        }
        return Some("tool_error");
    }
    None
}

fn record_tool(summary: &mut HarnessTraceSummary, tool: &str) {
    let normalized_tool = safe_tool_name(tool);
    let tool = if summary.tool_counts.contains_key(normalized_tool)
        || summary.tool_counts.len() < MAX_DISTINCT_TOOL_LABELS.saturating_sub(1)
    {
        normalized_tool
    } else {
        OTHER_TOOL_LABEL
    };
    summary.tool_calls = summary.tool_calls.saturating_add(1);
    let count = summary.tool_counts.entry(tool.to_owned()).or_default();
    if *count > 0 {
        summary.repeated_calls = summary.repeated_calls.saturating_add(1);
        let repeated_count = summary.repeated_tool_counts.entry(tool.to_owned()).or_default();
        *repeated_count = repeated_count.saturating_add(1);
    }
    *count = count.saturating_add(1);
}

fn add_error(summary: &mut HarnessTraceSummary, category: &str) {
    let category = canonical_error_category(category);
    let count = summary.error_categories.entry(category.to_owned()).or_default();
    *count = count.saturating_add(1);
}

fn safe_tool_name(tool: &str) -> &'static str {
    match tool.trim() {
        "apply_patch" => "apply_patch",
        "bash" => "bash",
        "code_search" => "code_search",
        "command_execution" => "command_execution",
        "create_goal" => "create_goal",
        "edit_file" => "edit_file",
        "edit" => "edit",
        "exec" => "exec",
        "exec_command" => "exec_command",
        "exec_pty_cmd" => "exec_pty_cmd",
        "fetch" => "fetch",
        "fetch_url" => "fetch_url",
        "get_goal" => "get_goal",
        "grep" => "grep",
        "grep_file" => "grep_file",
        "job_output" => "job_output",
        "list" => "list",
        "list_agents" => "list_agents",
        "list_dir" => "list_dir",
        "list_files" => "list_files",
        "mcp" => "mcp",
        "mcp_tool_call" => "mcp_tool_call",
        "read" => "read",
        "read_file" => "read_file",
        "search" => "search",
        "shell" => "shell",
        "skill" => "skill",
        "subagent" => "subagent",
        "task_tracker" => "task_tracker",
        "todo_write" => "todo_write",
        "update_goal" => "update_goal",
        "web_fetch" => "web_fetch",
        "web_search" => "web_search",
        "write" => "write",
        "write_file" => "write_file",
        "write_stdin" => "write_stdin",
        _ => OTHER_TOOL_LABEL,
    }
}

fn string_field<'a>(object: &'a serde_json::Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| object.get(*name).and_then(Value::as_str))
}

fn number_field(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| object.get(*name).and_then(Value::as_u64))
}

fn nested_number_field(object: &serde_json::Map<String, Value>, containers: &[&str], names: &[&str]) -> Option<u64> {
    containers.iter().find_map(|container| {
        object
            .get(*container)
            .and_then(Value::as_object)
            .and_then(|details| number_field(details, names))
    })
}

fn output_bytes(object: &serde_json::Map<String, Value>) -> Option<u64> {
    ["output", "aggregated_output", "tool_output"]
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str).map(|output| output.len() as u64))
}

fn error_category<'a>(
    primary: &'a serde_json::Map<String, Value>,
    fallback: &'a serde_json::Map<String, Value>,
) -> Option<&'a str> {
    if let Some(category) = string_field_from(primary, fallback, &["error_category", "error_code"]) {
        return Some(category);
    }
    match primary.get("error").or_else(|| fallback.get("error")) {
        Some(Value::String(error)) => Some(error),
        Some(Value::Object(error)) => string_field(error, &["category", "code", "type"]),
        _ => None,
    }
}

fn canonical_error_category(error: &str) -> &'static str {
    let error = error.trim().to_ascii_lowercase();
    if error.contains("fs_not_observed") || error.contains("fs-not-observed") {
        "fs_not_observed"
    } else if error.contains("fs_stale_version") || error.contains("fs-stale-version") {
        "fs_stale_version"
    } else if error.contains("unknown_job") || error.contains("unknown-job") || error.contains("unknown job") {
        "unknown_job"
    } else if error.contains("invalid_goal") || error.contains("invalid-goal") || error.contains("invalid goal") {
        "invalid_goal_update"
    } else if error.contains("timeout") || error.contains("timed out") {
        "timeout"
    } else if error.contains("permission") || error.contains("denied") {
        "permission_denied"
    } else if error.contains("network") || error.contains("connection") {
        "network"
    } else if error.contains("parse") || error.contains("json") {
        "parse"
    } else if error.contains("rate_limit") || error.contains("rate-limit") || error.contains("ratelimit") {
        "rate_limit"
    } else if error.contains("command_failed") || error.contains("command-failed") {
        "command_failed"
    } else if error.contains("tool_error") || error.contains("tool-error") {
        "tool_error"
    } else {
        GENERIC_ERROR_CATEGORY
    }
}

fn usage_value(object: &serde_json::Map<String, Value>) -> Option<&serde_json::Map<String, Value>> {
    ["usage", "tokens"]
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_object))
}

fn has_usage(object: &serde_json::Map<String, Value>) -> bool {
    usage_value(object).is_some()
        || object.keys().any(|key| {
            key.ends_with("_tokens")
                || matches!(key.as_str(), "inputTokens" | "outputTokens" | "cacheReadTokens" | "reasoningTokens")
        })
}

fn usage_sample(object: &serde_json::Map<String, Value>) -> Option<TokenUsage> {
    if !has_usage(object) {
        return None;
    }

    let source = usage_value(object).unwrap_or(object);
    Some(TokenUsage {
        input_tokens: number_field(source, &["input", "input_tokens", "prompt_tokens", "inputTokens"]).unwrap_or(0),
        output_tokens: number_field(source, &["output", "output_tokens", "completion_tokens", "outputTokens"])
            .unwrap_or(0),
        cached_input_tokens: number_field(
            source,
            &[
                "cached",
                "cached_tokens",
                "cached_input_tokens",
                "cacheReadTokens",
                "cache_read_tokens",
                "prompt_cache_hit_tokens",
            ],
        )
        .or_else(|| {
            nested_number_field(
                source,
                &["input_tokens_details", "prompt_tokens_details"],
                &["cached_tokens", "cache_read_tokens", "cacheReadTokens"],
            )
        })
        .unwrap_or(0),
        cache_creation_tokens: number_field(
            source,
            &[
                "cache_creation",
                "cache_creation_tokens",
                "cacheCreationTokens",
                "prompt_cache_creation_tokens",
                "cache_write_tokens",
            ],
        )
        .or_else(|| {
            nested_number_field(
                source,
                &["input_tokens_details", "prompt_tokens_details"],
                &["cache_creation_tokens", "cache_write_tokens", "cacheWriteTokens"],
            )
        })
        .unwrap_or(0),
        reasoning_tokens: number_field(source, &["reasoning", "reasoning_tokens", "reasoningTokens"])
            .or_else(|| {
                nested_number_field(
                    source,
                    &["output_tokens_details", "completion_tokens_details"],
                    &["reasoning_tokens", "reasoningTokens"],
                )
            })
            .unwrap_or(0),
    })
}

fn add_usage(total: &mut TokenUsage, sample: &TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_add(sample.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(sample.output_tokens);
    total.cached_input_tokens = total.cached_input_tokens.saturating_add(sample.cached_input_tokens);
    total.cache_creation_tokens = total.cache_creation_tokens.saturating_add(sample.cache_creation_tokens);
    total.reasoning_tokens = total.reasoning_tokens.saturating_add(sample.reasoning_tokens);
}

fn deterministic_reservoir_index(sample_number: u64) -> u64 {
    let mut mixed = sample_number;
    mixed ^= mixed >> 30;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    mixed = mixed.wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    mixed % sample_number.max(1)
}

#[cfg(test)]
mod latency_tests {
    use super::{analyse_jsonl, metrics::MAX_LATENCY_SAMPLES};

    #[test]
    fn bounds_latency_sample_storage_while_retaining_all_counts() {
        let input = (0..(MAX_LATENCY_SAMPLES * 2))
            .map(|latency| format!(r#"{{"latency_ms":{latency}}}"#))
            .collect::<Vec<_>>()
            .join("\n");

        let summary = analyse_jsonl(&input).expect("trace should parse");

        assert_eq!(summary.latency.count, (MAX_LATENCY_SAMPLES * 2) as u64);
        assert_eq!(summary.latency.max_ms, Some((MAX_LATENCY_SAMPLES * 2 - 1) as u64));
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;

    const THREAD_EVENT_TRACE: &str = r#"{"type":"turn.started"}
{"type":"item.completed","item":{"id":"1","type":"tool_invocation","tool_name":"exec_command","status":"completed"}}
{"type":"item.completed","item":{"id":"2","type":"tool_invocation","tool_name":"exec_command","status":"failed","outcome":"timeout"}}
{"type":"turn.completed","usage":{"input_tokens":8,"cached_input_tokens":3,"cache_creation_tokens":1,"output_tokens":5}}
{"type":"item.completed","item":{"id":"3","type":"tool_output","output":"private output"}}
"#;

    #[test]
    fn summarizes_deepseek_baseline_without_retaining_raw_text() {
        let trace = r#"
{"step":1,"tool":"exec_command","latency_ms":12,"output":"secret command output","tokens":{"input":100,"output":20,"cached":40}}
{"step":2,"tool":"read_file","latency_ms":20,"error":"timeout"}
"#;

        let summary = analyse_jsonl(trace).expect("trace should parse");

        assert_eq!(summary.steps, 2);
        assert_eq!(summary.tool_calls, 2);
        assert_eq!(summary.tool_counts["exec_command"], 1);
        assert_eq!(summary.tool_counts["read_file"], 1);
        assert_eq!(summary.error_categories["timeout"], 1);
        assert_eq!(summary.output_bytes, 21);
        assert_eq!(summary.token_usage.input_tokens, 100);
        assert_eq!(summary.token_usage.output_tokens, 20);
        assert_eq!(summary.token_usage.cached_input_tokens, 40);
        assert!(
            !serde_json::to_string(&summary)
                .expect("summary should serialize")
                .contains("secret command output")
        );
    }

    #[test]
    fn matches_known_deepseek_baseline_counts_with_compact_fixture() {
        let mut trace = String::new();
        for step in 1..=453 {
            trace.push_str(&format!(
                r#"{{"step":{step},"tool":"exec_command"}}
"#
            ));
        }
        trace.push_str(
            r#"{"tool":"exec_command"}
{"tool":"read_file"}
{"tool":"read_file"}
{"tool":"write_file"}
{"tool":"write_file"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"tool":"search"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
{"error":"timeout"}
"#,
        );

        let summary = analyse_jsonl(&trace).expect("baseline fixture should parse");

        assert_eq!(summary.steps, 453);
        assert_eq!(summary.tool_calls, 468);
        assert_eq!(summary.error_categories.values().sum::<u64>(), 20);
    }

    #[test]
    fn parses_thread_events_and_skips_bad_or_unknown_lines() {
        let input = format!("{}\nnot json\n{{\"future\":true}}\n", THREAD_EVENT_TRACE);
        let summary = analyse_jsonl(&input).expect("trace should parse");

        assert_eq!(summary.turns, 1);
        assert_eq!(summary.steps, 2);
        assert_eq!(summary.tool_calls, 2);
        assert_eq!(summary.repeated_calls, 1);
        assert_eq!(summary.error_categories["timeout"], 1);
        assert_eq!(summary.output_bytes, 14);
        assert_eq!(summary.token_usage.input_tokens, 8);
        assert_eq!(summary.token_usage.cached_input_tokens, 3);
        assert_eq!(summary.malformed_lines, 1);
        assert_eq!(summary.unrecognized_lines, 1);
    }

    #[test]
    fn reports_latency_statistics_and_file_errors_with_context() {
        let summary = analyse_jsonl(
            "{\"step\":1,\"latency_ms\":30}\n{\"step\":2,\"latency_ms\":10}\n{\"step\":3,\"latency_ms\":20}\n",
        )
        .expect("latency trace should parse");
        assert_eq!(summary.latency.count, 3);
        assert_eq!(summary.latency.total_ms, 60);
        assert_eq!(summary.latency.p50_ms, Some(20));
        assert_eq!(summary.latency.p95_ms, Some(30));

        let missing = analyse_jsonl_file("/path/that/does/not/exist.jsonl").expect_err("missing file should fail");
        assert!(missing.to_string().contains("read trace file"));
    }

    #[test]
    fn redacts_untrusted_labels_and_counts_event_errors_once() {
        let input = r#"{"type":"error","error":"secret command output timeout"}
{"type":"turn.failed","error_category":"FS_STALE_VERSION","error":"private details"}
{"step":1,"tool":"rm /sensitive/project","error":{"type":"secret_error"}}
"#;

        let summary = analyse_jsonl(input).expect("trace should parse");
        let serialized = serde_json::to_string(&summary).expect("summary should serialize");

        assert_eq!(summary.error_categories.values().sum::<u64>(), 3);
        assert_eq!(summary.error_categories["timeout"], 1);
        assert_eq!(summary.error_categories["fs_stale_version"], 1);
        assert_eq!(summary.error_categories["error"], 1);
        assert_eq!(summary.tool_counts["other_tool"], 1);
        assert!(!serialized.contains("secret command output"));
        assert!(!serialized.contains("/sensitive/project"));
        assert!(!serialized.contains("secret_error"));
    }

    #[test]
    fn classifies_unrelated_goal_text_by_its_actual_error() {
        let summary = analyse_jsonl(
            r#"{"error":"goal timeout"}
{"error":"invalid goal update"}
"#,
        )
        .expect("trace should parse");

        assert_eq!(summary.error_categories["timeout"], 1);
        assert_eq!(summary.error_categories["invalid_goal_update"], 1);
    }

    #[test]
    fn saturates_latency_total_on_overflow() {
        let input = format!("{{\"latency_ms\":{0}}}\n{{\"latency_ms\":{0}}}\n", u64::MAX);

        let summary = analyse_jsonl(&input).expect("trace should parse");

        assert_eq!(summary.latency.total_ms, u64::MAX);
        assert_eq!(summary.latency.max_ms, Some(u64::MAX));
    }

    #[test]
    fn prefers_per_turn_usage_over_thread_aggregate_and_reads_thread_turn_count() {
        let input = r#"{"type":"turn.completed","usage":{"input_tokens":3,"cached_input_tokens":1,"output_tokens":2}}
{"type":"thread.completed","num_turns":7,"usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":8},"result":"private assistant result"}
"#;

        let summary = analyse_jsonl(input).expect("trace should parse");

        assert_eq!(summary.turns, 7);
        assert_eq!(summary.token_usage.input_tokens, 3);
        assert_eq!(summary.token_usage.cached_input_tokens, 1);
        assert_eq!(summary.token_usage.output_tokens, 2);
        assert_eq!(summary.output_bytes, 0);
    }

    #[test]
    fn falls_back_to_thread_aggregate_usage_when_turn_usage_is_missing() {
        let input = r#"{"type":"thread.completed","num_turns":4,"usage":{"input_tokens":10,"cached_input_tokens":4,"cache_creation_tokens":2,"output_tokens":8}}
"#;

        let summary = analyse_jsonl(input).expect("trace should parse");

        assert_eq!(summary.turns, 4);
        assert_eq!(summary.token_usage.input_tokens, 10);
        assert_eq!(summary.token_usage.cached_input_tokens, 4);
        assert_eq!(summary.token_usage.cache_creation_tokens, 2);
        assert_eq!(summary.token_usage.output_tokens, 8);
    }

    #[test]
    fn buffered_reader_api_matches_text_analysis() {
        let input = "{\"step\":1,\"tool\":\"read_file\",\"latency_ms\":12}\n";

        let from_text = analyse_jsonl(input).expect("text trace should parse");
        let from_reader =
            analyse_jsonl_reader(BufReader::new(Cursor::new(input.as_bytes()))).expect("buffered trace should parse");

        assert_eq!(from_reader, from_text);
    }

    #[test]
    fn parses_nested_deepseek_envelopes_and_camel_case_usage() {
        let input = r#"{"type":"turn/start","time":100,"data":{"turn":1}}
{"type":"step/start","time":110,"data":{"step":1,"turn":1}}
{"type":"assistant/message","data":{"step":1,"usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":40,"reasoningTokens":7}}}
{"type":"tool/call","time":120,"data":{"name":"read_file","arguments":"private arguments","step":1,"turn":1}}
{"type":"tool/result","time":150,"data":{"step":1,"turn":1,"message":{"content":[{"type":"text","content":["private output"],"isError":true}]}}}
{"type":"step/end","time":180,"data":{"step":1,"turn":1}}
{"type":"turn/end","time":200,"data":{"turn":1,"reason":{}}}
"#;

        let summary = analyse_jsonl(input).expect("nested trace should parse");
        let serialized = serde_json::to_string(&summary).expect("summary should serialize");

        assert_eq!(summary.turns, 1);
        assert_eq!(summary.steps, 1);
        assert_eq!(summary.tool_calls, 1);
        assert_eq!(summary.tool_counts["read_file"], 1);
        assert_eq!(summary.error_categories["tool_error"], 1);
        assert_eq!(summary.output_bytes, 14);
        assert_eq!(summary.latency.total_ms, 70);
        assert_eq!(summary.token_usage.input_tokens, 100);
        assert_eq!(summary.token_usage.output_tokens, 20);
        assert_eq!(summary.token_usage.cached_input_tokens, 40);
        assert_eq!(summary.token_usage.reasoning_tokens, 7);
        assert!(!serialized.contains("private arguments"));
        assert!(!serialized.contains("private output"));
    }

    #[test]
    fn parses_versioned_vtcode_event_envelopes() {
        let input = r#"{"schema_version":"0.11.0","event":{"type":"turn.started"}}
{"schema_version":"0.11.0","event":{"type":"item.completed","item":{"id":"1","type":"tool_invocation","tool_name":"read_file","status":"completed"}}}
{"schema_version":"0.11.0","event":{"type":"turn.completed","usage":{"input_tokens":8,"output_tokens":2}}}
"#;

        let summary = analyse_jsonl(input).expect("versioned trace should parse");

        assert_eq!(summary.turns, 1);
        assert_eq!(summary.steps, 1);
        assert_eq!(summary.tool_counts["read_file"], 1);
        assert_eq!(summary.token_usage.input_tokens, 8);
        assert_eq!(summary.token_usage.output_tokens, 2);
        assert_eq!(summary.unrecognized_lines, 0);
    }

    #[test]
    fn terminal_usage_takes_precedence_over_intermediate_and_thread_usage() {
        let input = r#"{"type":"assistant/message","data":{"usage":{"inputTokens":100,"outputTokens":20}}}
{"type":"turn/end","data":{"usage":{"inputTokens":3,"outputTokens":2}}}
{"type":"thread.completed","num_turns":1,"usage":{"input_tokens":10,"output_tokens":8}}
"#;

        let summary = analyse_jsonl(input).expect("usage trace should parse");

        assert_eq!(summary.token_usage.input_tokens, 3);
        assert_eq!(summary.token_usage.output_tokens, 2);
    }

    #[test]
    fn parses_nested_provider_token_details() {
        let input = r#"{"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_cache_hit_tokens":40,"input_tokens_details":{"cached_tokens":5},"output_tokens_details":{"reasoning_tokens":7}}}
"#;

        let summary = analyse_jsonl(input).expect("provider usage should parse");

        assert_eq!(summary.token_usage.input_tokens, 100);
        assert_eq!(summary.token_usage.output_tokens, 20);
        assert_eq!(summary.token_usage.cached_input_tokens, 40);
        assert_eq!(summary.token_usage.reasoning_tokens, 7);
    }

    #[test]
    fn rejects_oversized_records_without_blocking_following_lines() {
        let input = format!(
            "{{\"payload\":\"{}\"}}\n{{\"step\":1,\"tool\":\"read_file\"}}\n",
            "x".repeat(MAX_TRACE_LINE_BYTES)
        );

        let summary = analyse_jsonl(&input).expect("oversized trace should parse");

        assert_eq!(summary.malformed_lines, 1);
        assert_eq!(summary.steps, 1);
        assert_eq!(summary.tool_calls, 1);
    }
}
