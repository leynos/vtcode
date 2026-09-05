//! Checked Responses usage decoding. Unknown usage is not measured zero usage.

use crate::provider::{LLMError, Usage};
use serde_json::Value;

fn invalid_usage(field: &str) -> LLMError {
    LLMError::Provider {
        message: format!("Responses usage field `{field}` must be an unsigned 32-bit token count"),
        metadata: None,
    }
}

fn counter(value: Option<&Value>, field: &str) -> Result<Option<u32>, LLMError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .map(Some)
            .ok_or_else(|| invalid_usage(field)),
    }
}

fn first_present<'a>(value: &'a Value, paths: &[&str]) -> Option<&'a Value> {
    paths
        .iter()
        .find_map(|path| value.pointer(path).filter(|value| !value.is_null()))
}

/// Decode the measured usage object without wrapping or replacing bad counts with zero.
/// Required input/output counts must both be present. An omitted total is derived
/// using checked addition; absent optional detail counters remain absent.
pub(crate) fn parse_usage(response: &Value, include_cached: bool) -> Result<Option<Usage>, LLMError> {
    let Some(usage) = response.get("usage").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if !usage.is_object() {
        return Err(invalid_usage("usage"));
    }
    let prompt_tokens = counter(first_present(usage, &["/input_tokens", "/prompt_tokens"]), "input_tokens")?
        .ok_or_else(|| invalid_usage("input_tokens"))?;
    let completion_tokens = counter(first_present(usage, &["/output_tokens", "/completion_tokens"]), "output_tokens")?
        .ok_or_else(|| invalid_usage("output_tokens"))?;
    let total_tokens = match counter(usage.get("total_tokens"), "total_tokens")? {
        Some(total) => total,
        None => prompt_tokens
            .checked_add(completion_tokens)
            .ok_or_else(|| invalid_usage("total_tokens"))?,
    };
    let reasoning_output_tokens = counter(
        first_present(
            usage,
            &[
                "/output_tokens_details/reasoning_tokens",
                "/completion_tokens_details/reasoning_tokens",
            ],
        ),
        "reasoning_tokens",
    )?;
    let cached_prompt_tokens = if include_cached {
        counter(
            first_present(
                usage,
                &[
                    "/input_tokens_details/cached_tokens",
                    "/prompt_tokens_details/cached_tokens",
                    "/prompt_cache_hit_tokens",
                    "/cached_tokens",
                ],
            ),
            "cached_tokens",
        )?
    } else {
        None
    };
    let cache_creation_tokens = if include_cached {
        counter(
            first_present(
                usage,
                &[
                    "/input_tokens_details/cache_write_tokens",
                    "/prompt_tokens_details/cache_write_tokens",
                ],
            ),
            "cache_write_tokens",
        )?
    } else {
        None
    };
    Ok(Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        reasoning_output_tokens,
        cached_prompt_tokens,
        cache_creation_tokens,
        cache_read_tokens: None,
        iterations: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::parse_usage;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn missing_and_null_usage_are_not_zero_usage() {
        assert!(parse_usage(&json!({}), true).unwrap().is_none());
        assert!(parse_usage(&json!({"usage":null}), true).unwrap().is_none());
        assert!(parse_usage(&json!({"usage":{}}), true).is_err());
        let zero = parse_usage(&json!({"usage":{"input_tokens":0,"output_tokens":0}}), true)
            .unwrap()
            .unwrap();
        assert_eq!(zero.total_tokens, 0);
        assert_eq!(zero.reasoning_output_tokens, None);
        assert_eq!(zero.cached_prompt_tokens, None);
        assert_eq!(zero.cache_creation_tokens, None);
    }

    #[test]
    fn cache_write_tokens_support_responses_and_chat_shapes_and_metrics_gate() {
        for details_field in ["input_tokens_details", "prompt_tokens_details"] {
            let mut response = json!({"usage":{
                "input_tokens":1,
                "output_tokens":2
            }});
            response["usage"][details_field] = json!({"cache_write_tokens":58});
            assert_eq!(parse_usage(&response, true).unwrap().unwrap().cache_creation_tokens, Some(58));
            assert_eq!(parse_usage(&response, false).unwrap().unwrap().cache_creation_tokens, None);
        }
    }

    #[test]
    fn overflowing_and_malformed_counts_are_errors_not_zero() {
        for value in [
            json!(u64::from(u32::MAX) + 1),
            json!(u64::MAX),
            json!(-1),
            json!(1.5),
            json!("12"),
        ] {
            assert!(parse_usage(&json!({"usage":{"input_tokens":value,"output_tokens":0}}), true).is_err());
        }
        assert!(parse_usage(&json!({"usage":{"input_tokens":u32::MAX,"output_tokens":1}}), true).is_err());
        assert!(parse_usage(&json!({"usage":{"input_tokens":0,"output_tokens":0,"output_tokens_details":{"reasoning_tokens":u64::MAX}}}), true).is_err());
        for value in [json!(u64::from(u32::MAX) + 1), json!(-1), json!("12")] {
            assert!(parse_usage(&json!({"usage":{"input_tokens":0,"output_tokens":0,"input_tokens_details":{"cache_write_tokens":value}}}), true).is_err());
        }
    }

    proptest! {
        #[test]
        fn representable_usage_is_exact(
            input in any::<u32>(),
            output in any::<u32>(),
            reasoning in any::<u32>(),
            cached in any::<u32>(),
            cache_write in any::<u32>(),
        ) {
            let result = parse_usage(&json!({"usage":{
                "input_tokens":input,"output_tokens":output,
                "input_tokens_details":{"cached_tokens":cached,"cache_write_tokens":cache_write},
                "output_tokens_details":{"reasoning_tokens":reasoning}
            }}), true);
            match input.checked_add(output) {
                Some(total) => {
                    let usage = result.unwrap().unwrap();
                    prop_assert_eq!(usage.prompt_tokens, input);
                    prop_assert_eq!(usage.completion_tokens, output);
                    prop_assert_eq!(usage.total_tokens, total);
                    prop_assert_eq!(usage.reasoning_output_tokens, Some(reasoning));
                    prop_assert_eq!(usage.cached_prompt_tokens, Some(cached));
                    prop_assert_eq!(usage.cache_creation_tokens, Some(cache_write));
                }
                None => prop_assert!(result.is_err()),
            }
        }

        #[test]
        fn unrepresentable_usage_never_becomes_zero(count in (u64::from(u32::MAX) + 1)..=u64::MAX) {
            let result = parse_usage(&json!({"usage":{"input_tokens":count,"output_tokens":0}}), true);
            prop_assert!(result.is_err());
        }
    }
}
