//! Shared model-visible tool-output preview limits.

use anyhow::{Context, Result, anyhow};
use diapason_utility_tool_specs::{
    DEFAULT_MAX_OUTPUT_TOKENS, MAX_MAX_OUTPUT_TOKENS, MAX_OUTPUT_TOKENS_FIELD, MIN_MAX_OUTPUT_TOKENS,
};
use serde_json::Value;

pub(crate) const OUTPUT_PREVIEW_CHARS_PER_TOKEN: usize = 4;

/// Validates and returns the requested model-visible result preview budget.
///
/// A missing value resolves to the stable default. Values must be JSON integers
/// so callers cannot silently coerce floats or strings into a larger context
/// allocation than the model requested.
pub(crate) fn max_output_tokens(args: &Value) -> Result<usize> {
    let Some(value) = args.get(MAX_OUTPUT_TOKENS_FIELD) else {
        return Ok(DEFAULT_MAX_OUTPUT_TOKENS);
    };
    let Some(tokens) = value.as_u64() else {
        return Err(anyhow!(
            "max_output_tokens must be an integer between {MIN_MAX_OUTPUT_TOKENS} and {MAX_MAX_OUTPUT_TOKENS}"
        ));
    };
    let tokens = usize::try_from(tokens).with_context(|| {
        format!("max_output_tokens must be an integer between {MIN_MAX_OUTPUT_TOKENS} and {MAX_MAX_OUTPUT_TOKENS}")
    })?;
    if !(MIN_MAX_OUTPUT_TOKENS..=MAX_MAX_OUTPUT_TOKENS).contains(&tokens) {
        return Err(anyhow!(
            "max_output_tokens must be an integer between {MIN_MAX_OUTPUT_TOKENS} and {MAX_MAX_OUTPUT_TOKENS}"
        ));
    }
    Ok(tokens)
}

/// Removes execution-only output metadata before validating a legacy schema.
/// The field is retained in the original payload and reaches the execution
/// pipeline, but schemas authored before this common option remain strict.
pub(crate) fn args_without_output_metadata(args: &Value) -> Value {
    let mut args = args.clone();
    if let Some(object) = args.as_object_mut() {
        object.remove(MAX_OUTPUT_TOKENS_FIELD);
    }
    args
}

/// Whether a handler explicitly consumes the common output-limit field.
pub(crate) fn handler_accepts_output_metadata(schema: Option<&Value>) -> bool {
    schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key(MAX_OUTPUT_TOKENS_FIELD))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn max_output_tokens_defaults_and_enforces_integer_bounds() {
        assert_eq!(max_output_tokens(&json!({})).unwrap(), DEFAULT_MAX_OUTPUT_TOKENS);
        assert_eq!(max_output_tokens(&json!({"max_output_tokens": 1})).unwrap(), 1);
        assert_eq!(max_output_tokens(&json!({"max_output_tokens": 50_000})).unwrap(), 50_000);
        assert!(max_output_tokens(&json!({"max_output_tokens": 0})).is_err());
        assert!(max_output_tokens(&json!({"max_output_tokens": 50_001})).is_err());
        assert!(max_output_tokens(&json!({"max_output_tokens": 1.0})).is_err());
        assert!(max_output_tokens(&json!({"max_output_tokens": "100"})).is_err());
    }

    #[test]
    fn output_metadata_is_removed_only_from_schema_validation_copy() {
        let original = json!({"input": "value", "max_output_tokens": 23});
        assert_eq!(args_without_output_metadata(&original), json!({"input": "value"}));
        assert_eq!(original["max_output_tokens"], 23);
    }

    #[test]
    fn handler_metadata_forwarding_requires_an_explicit_schema_field() {
        assert!(!handler_accepts_output_metadata(Some(&json!({"type": "object"}))));
        assert!(handler_accepts_output_metadata(Some(&json!({
            "type": "object",
            "properties": {"max_output_tokens": {"type": "integer"}}
        }))));
    }
}
