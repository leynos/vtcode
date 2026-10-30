//! Validate explicit provider terminal state before accepting a completion.

use crate::error_display;
use crate::provider::LLMError;
use serde_json::Value;

pub(crate) fn validate_completed_response(response: &Value) -> Result<(), LLMError> {
    match response.get("status") {
        None => Ok(()),
        Some(Value::String(status)) if status == "completed" => Ok(()),
        Some(status) => {
            let reason = response
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| response.pointer("/incomplete_details/reason").and_then(Value::as_str))
                .unwrap_or("expected a completed response");
            Err(LLMError::Provider {
                message: error_display::format_llm_error(
                    "Responses",
                    &format!("Response has non-completed status {status}: {reason}"),
                ),
                metadata: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_completed_response;
    use serde_json::json;

    #[test]
    fn explicit_incomplete_status_is_not_a_completion() {
        for status in [
            json!("incomplete"),
            json!("failed"),
            json!("in_progress"),
            json!("cancelled"),
            json!(null),
            json!(1),
        ] {
            let error = validate_completed_response(
                &json!({"status":status,"incomplete_details":{"reason":"max_output_tokens"}}),
            )
            .unwrap_err();
            assert!(error.to_string().contains("max_output_tokens"));
        }
        assert!(validate_completed_response(&json!({"status":"completed"})).is_ok());
        assert!(validate_completed_response(&json!({})).is_ok());
    }
}
