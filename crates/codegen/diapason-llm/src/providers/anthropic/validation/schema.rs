//! Structured-output schema validation for the Anthropic Claude API.

use crate::error_display;
use crate::provider::LLMError;

pub fn validate_anthropic_schema(schema: &serde_json::Value, _provider_name: &str) -> Result<(), LLMError> {
    use serde_json::Value;

    match schema {
        Value::Object(obj) => {
            validate_schema_object(obj, "root")?;
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Array(_) | Value::Null => {
            let formatted_error =
                error_display::format_llm_error("Anthropic", "Structured output schema must be a JSON object");
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }
    }
    Ok(())
}

fn validate_schema_object(obj: &serde_json::Map<String, serde_json::Value>, path: &str) -> Result<(), LLMError> {
    use serde_json::Value;

    for (key, value) in obj {
        match key.as_str() {
            "type" => {
                if let Some(type_str) = value.as_str() {
                    match type_str {
                        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null" => {}
                        _ => {
                            let formatted_error = error_display::format_llm_error(
                                "Anthropic",
                                &format!("Unsupported schema type '{type_str}', path: {path}"),
                            );
                            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
                        }
                    }
                }
            }
            "minimum" | "maximum" | "multipleOf" => {
                let formatted_error = error_display::format_llm_error(
                    "Anthropic",
                    &format!(
                        "Numeric constraints like '{key}' are not supported by Anthropic structured output. Path: {path}"
                    ),
                );
                return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
            }
            "minLength" | "maxLength" => {
                let formatted_error = error_display::format_llm_error(
                    "Anthropic",
                    &format!(
                        "String constraints like '{key}' are not supported by Anthropic structured output. Path: {path}"
                    ),
                );
                return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
            }
            "minItems" | "maxItems" | "uniqueItems" => {
                if key == "minItems" {
                    if let Some(min_items) = value.as_u64()
                        && min_items > 1
                    {
                        let formatted_error = error_display::format_llm_error(
                            "Anthropic",
                            &format!("Array minItems only supports values 0 or 1, got {min_items}, path: {path}"),
                        );
                        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
                    }
                } else {
                    let formatted_error = error_display::format_llm_error(
                        "Anthropic",
                        &format!(
                            "Array constraints like '{key}' are not supported by Anthropic structured output. Path: {path}"
                        ),
                    );
                    return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
                }
            }
            "additionalProperties" => {
                if let Some(additional_props) = value.as_bool()
                    && additional_props
                {
                    let formatted_error = error_display::format_llm_error(
                        "Anthropic",
                        &format!("additionalProperties must be set to false, got {additional_props}, path: {path}"),
                    );
                    return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
                }
            }
            "properties" => {
                if let Value::Object(props) = value {
                    for (prop_name, prop_value) in props {
                        let prop_path = format!("{path}.properties.{prop_name}");
                        if let Value::Object(prop_obj) = prop_value {
                            validate_schema_object(prop_obj, &prop_path)?;
                        }
                    }
                }
            }
            "items" => {
                if let Value::Object(items_obj) = value {
                    let items_path = format!("{path}.items");
                    validate_schema_object(items_obj, &items_path)?;
                }
            }
            "anyOf" | "allOf" | "oneOf" => {
                if let Value::Array(options) = value {
                    for (i, option) in options.iter().enumerate() {
                        if let Value::Object(option_obj) = option {
                            let option_path = format!("{path}.{key}[{i}]");
                            validate_schema_object(option_obj, &option_path)?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
