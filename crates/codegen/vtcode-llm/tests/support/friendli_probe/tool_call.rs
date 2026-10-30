//! Terminal tool-call identity checks and synthetic probe-input validation.

use anyhow::Result;
use serde_json::{Value, json};

use super::ProbeOutcome;

/// Checks one terminal call's type, name, final call ID, and expected input.
///
/// Identity checks precede input decoding so malformed calls retain the probe's
/// established failure priority. Success requires a nonempty final call ID.
pub(super) fn classify_tool_call(call: &Value, expected_type: &str, expected_name: &str) -> ProbeOutcome {
    if call["type"] != expected_type {
        return ProbeOutcome::DecodeFailure {
            error: format!("tool call type {:?} did not match {expected_type:?}", call["type"]),
        };
    }
    let Some(name) = call["name"].as_str() else {
        return ProbeOutcome::DecodeFailure { error: "tool call omitted name".into() };
    };
    let Some(call_id) = call["call_id"].as_str() else {
        return ProbeOutcome::DecodeFailure { error: "tool call omitted final call_id".into() };
    };
    if call_id.is_empty() {
        return ProbeOutcome::DecodeFailure {
            error: "tool call had an empty final call_id".into(),
        };
    }
    if name != expected_name {
        return ProbeOutcome::DecodeFailure {
            error: format!("tool call name {name:?} did not match {expected_name:?}"),
        };
    }
    let input = match decode_expected_tool_input(call, expected_type) {
        Ok(input) => input,
        Err(error) => return ProbeOutcome::DecodeFailure { error },
    };
    ProbeOutcome::ToolCallSuccess {
        tool_type: expected_type.into(),
        name: name.into(),
        call_id: call_id.into(),
        input,
    }
}

/// Decodes the synthetic input required by the selected tool-call format.
///
/// Function arguments must be a strict JSON string encoding exactly
/// `{"text":"OK"}`; custom-tool input must be the raw string `OK`. Missing custom
/// input is treated as null and rejected with the same diagnostic as other
/// unexpected input values.
fn decode_expected_tool_input(call: &Value, expected_type: &str) -> Result<Value, String> {
    let input = if expected_type == "function_call" {
        call["arguments"]
            .as_str()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .ok_or_else(|| "function arguments were not strict JSON".to_string())?
    } else {
        call.get("input").cloned().unwrap_or(Value::Null)
    };
    let valid_input = if expected_type == "function_call" {
        input == json!({"text":"OK"})
    } else {
        input == "OK"
    };
    if !valid_input {
        return Err(format!("unexpected tool input: {input}"));
    }
    Ok(input)
}
