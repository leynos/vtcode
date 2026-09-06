use crate::provider::ToolCall;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolCallCorrelation {
    pub(crate) streamed_index: usize,
    pub(crate) final_index: usize,
}

/// Correlate streamed calls with terminal calls without mutating either list.
///
/// Exact IDs are authoritative. When the compatibility capability is enabled,
/// only unmatched ordinary function calls may be paired by a unique,
/// non-empty function name and strictly parsed JSON value.
pub(crate) fn correlate_streamed_function_calls(
    final_calls: &[ToolCall],
    streamed_calls: &[ToolCall],
    allow_function_call_id_remap: bool,
) -> Result<Vec<ToolCallCorrelation>, &'static str> {
    reject_duplicate_ids(final_calls)?;
    reject_duplicate_ids(streamed_calls)?;

    let mut correlations = Vec::with_capacity(streamed_calls.len());
    let mut final_matched = vec![false; final_calls.len()];
    let mut streamed_matched = vec![false; streamed_calls.len()];

    for (streamed_index, streamed_call) in streamed_calls.iter().enumerate() {
        if let Some((final_index, _)) = final_calls
            .iter()
            .enumerate()
            .find(|(_, final_call)| final_call.id == streamed_call.id)
        {
            final_matched[final_index] = true;
            streamed_matched[streamed_index] = true;
            correlations.push(ToolCallCorrelation { streamed_index, final_index });
        }
    }

    let unmatched_streamed = streamed_matched
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| (!matched).then_some(index))
        .collect::<Vec<_>>();
    if unmatched_streamed.is_empty() {
        return Ok(correlations);
    }
    if !allow_function_call_id_remap {
        return Err("completed response omitted a streamed tool call");
    }

    let unmatched_final = final_matched
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| (!matched).then_some(index))
        .collect::<Vec<_>>();
    if unmatched_final.len() != unmatched_streamed.len() {
        return Err("completed response tool calls do not map one-to-one to streamed calls");
    }

    let streamed_keys = semantic_keys(streamed_calls, &unmatched_streamed)?;
    let final_keys = semantic_keys(final_calls, &unmatched_final)?;
    reject_duplicate_semantic_keys(&streamed_keys)?;
    reject_duplicate_semantic_keys(&final_keys)?;

    for (streamed_offset, streamed_key) in streamed_keys.iter().enumerate() {
        let Some(final_offset) = final_keys.iter().position(|final_key| final_key == streamed_key) else {
            return Err("completed response tool calls contradict streamed calls");
        };
        correlations.push(ToolCallCorrelation {
            streamed_index: unmatched_streamed[streamed_offset],
            final_index: unmatched_final[final_offset],
        });
    }

    Ok(correlations)
}

fn reject_duplicate_ids(calls: &[ToolCall]) -> Result<(), &'static str> {
    for (index, call) in calls.iter().enumerate() {
        if call.id.is_empty() {
            return Err("tool call ID must not be empty");
        }
        if calls[..index].iter().any(|previous| previous.id == call.id) {
            return Err("tool call IDs must be unique");
        }
    }
    Ok(())
}

fn semantic_keys<'a>(
    calls: &'a [ToolCall],
    unmatched_indices: &[usize],
) -> Result<Vec<(&'a str, Value)>, &'static str> {
    unmatched_indices
        .iter()
        .map(|&index| {
            let call = &calls[index];
            if call.is_custom() || call.call_type != "function" {
                return Err("only ordinary function calls allow ID remapping");
            }
            let name = call
                .tool_name()
                .filter(|name| !name.is_empty())
                .ok_or("function call name must not be empty")?;
            let input = call.raw_input().ok_or("function call arguments are missing")?;
            let arguments = serde_json::from_str(input).map_err(|error| match error.classify() {
                // Preserve classification without echoing tool arguments in
                // serde's potentially content-bearing diagnostic text.
                serde_json::error::Category::Eof => "function call arguments contain truncated JSON",
                serde_json::error::Category::Syntax => "function call arguments contain invalid JSON syntax",
                serde_json::error::Category::Data => "function call arguments contain invalid JSON data",
                serde_json::error::Category::Io => "function call arguments could not be read as JSON",
            })?;
            Ok((name, arguments))
        })
        .collect()
}

fn reject_duplicate_semantic_keys(keys: &[(&str, Value)]) -> Result<(), &'static str> {
    for (index, key) in keys.iter().enumerate() {
        if keys[..index].iter().any(|previous| previous == key) {
            return Err("function-call ID remapping is ambiguous");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::correlate_streamed_function_calls;
    use crate::provider::ToolCall;
    use proptest::prelude::*;

    fn function(id: impl Into<String>, name: impl Into<String>, arguments: impl Into<String>) -> ToolCall {
        ToolCall::function(id.into(), name.into(), arguments.into())
    }

    #[test]
    fn opt_in_correlates_unique_semantic_calls_and_keeps_final_indices_authoritative() {
        let streamed = [function("stream-a", "alpha", r#"{"x":1,"y":2}"#)];
        let final_calls = [function("final-a", "alpha", r#"{"y":2,"x":1}"#)];

        let correlations = correlate_streamed_function_calls(&final_calls, &streamed, true).expect("unique match");
        assert_eq!(correlations[0].streamed_index, 0);
        assert_eq!(correlations[0].final_index, 0);
        assert_eq!(final_calls[correlations[0].final_index].id, "final-a");
    }

    #[test]
    fn strict_default_rejects_mismatched_ids() {
        let streamed = [function("stream-a", "alpha", "{}")];
        let final_calls = [function("final-a", "alpha", "{}")];
        assert!(correlate_streamed_function_calls(&final_calls, &streamed, false).is_err());
    }

    #[test]
    fn opt_in_rejects_custom_partial_duplicate_and_contradictory_calls() {
        let custom = [ToolCall::custom("stream".into(), "raw".into(), "payload".into())];
        let custom_final = [ToolCall::custom("final".into(), "raw".into(), "payload".into())];
        assert!(correlate_streamed_function_calls(&custom_final, &custom, true).is_err());

        let partial = [function("stream", "alpha", r#"{"x":"#)];
        let partial_final = [function("final", "alpha", r#"{"x":1}"#)];
        assert_eq!(
            correlate_streamed_function_calls(&partial_final, &partial, true),
            Err("function call arguments contain truncated JSON")
        );

        let duplicate_ids = [function("same", "alpha", "{}"), function("same", "beta", "{}")];
        assert!(correlate_streamed_function_calls(&duplicate_ids, &[], true).is_err());

        let contradictory = [function("stream", "alpha", "{}")];
        let contradictory_final = [function("final", "beta", "{}")];
        assert!(correlate_streamed_function_calls(&contradictory_final, &contradictory, true).is_err());
    }

    proptest! {
        #[test]
        fn generated_mixed_id_rewrites_preserve_semantic_bijection(
            (argument_texts, exact_first, rotation, reverse) in (2usize..=16).prop_flat_map(|length| (
                proptest::collection::vec(any::<String>(), length),
                any::<bool>(),
                any::<usize>(),
                any::<bool>(),
            )),
        ) {
            let streamed = argument_texts
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    function(
                        format!("stream-{index}"),
                        format!("tool-{index}"),
                        serde_json::json!({"text": text}).to_string(),
                    )
                })
                .collect::<Vec<_>>();
            let mut final_calls = argument_texts
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let exact = (index % 2 == 0) == exact_first;
                    function(
                        if exact { format!("stream-{index}") } else { format!("final-{index}") },
                        format!("tool-{index}"),
                        serde_json::json!({"text": text}).to_string(),
                    )
                })
                .collect::<Vec<_>>();
            let final_len = final_calls.len();
            final_calls.rotate_left(rotation % final_len);
            if reverse {
                final_calls.reverse();
            }

            let correlations = correlate_streamed_function_calls(&final_calls, &streamed, true)
                .map_err(TestCaseError::fail)?;
            prop_assert_eq!(correlations.len(), streamed.len());
            let mut seen_final = vec![false; final_calls.len()];
            for correlation in correlations {
                let streamed_call = &streamed[correlation.streamed_index];
                let final_call = &final_calls[correlation.final_index];
                prop_assert_eq!(streamed_call.tool_name(), final_call.tool_name());
                prop_assert_eq!(
                    serde_json::from_str::<serde_json::Value>(streamed_call.raw_input().unwrap_or_default())
                        .map_err(|error| TestCaseError::fail(error.to_string()))?,
                    serde_json::from_str::<serde_json::Value>(final_call.raw_input().unwrap_or_default())
                        .map_err(|error| TestCaseError::fail(error.to_string()))?,
                );
                prop_assert!(!seen_final[correlation.final_index]);
                seen_final[correlation.final_index] = true;
            }
            prop_assert!(seen_final.into_iter().all(|seen| seen));
        }

        #[test]
        fn generated_changed_payload_is_rejected(
            argument_texts in proptest::collection::vec(any::<String>(), 1..=16),
            changed_index_seed in any::<usize>(),
        ) {
            let changed_index = changed_index_seed % argument_texts.len();
            let streamed = argument_texts
                .iter()
                .enumerate()
                .map(|(index, text)| function(
                    format!("stream-{index}"),
                    format!("tool-{index}"),
                    serde_json::json!({"text": text}).to_string(),
                ))
                .collect::<Vec<_>>();
            let final_calls = argument_texts
                .iter()
                .enumerate()
                .map(|(index, text)| function(
                    format!("final-{index}"),
                    format!("tool-{index}"),
                    if index == changed_index {
                        serde_json::json!({"text": text, "changed": true}).to_string()
                    } else {
                        serde_json::json!({"text": text}).to_string()
                    },
                ))
                .collect::<Vec<_>>();

            let error = correlate_streamed_function_calls(&final_calls, &streamed, true)
                .expect_err("changed semantic payload must not remap");
            prop_assert_eq!(error, "completed response tool calls contradict streamed calls");
        }

        #[test]
        fn duplicated_semantic_payloads_are_always_ambiguous(value in any::<u64>()) {
            let arguments = serde_json::json!({"value": value}).to_string();
            let streamed = [
                function("stream-a", "same", arguments.clone()),
                function("stream-b", "same", arguments.clone()),
            ];
            let final_calls = [
                function("final-a", "same", arguments.clone()),
                function("final-b", "same", arguments),
            ];

            let error = correlate_streamed_function_calls(&final_calls, &streamed, true)
                .expect_err("duplicate semantic calls must be ambiguous");
            prop_assert_eq!(error, "function-call ID remapping is ambiguous");
        }
    }
}
