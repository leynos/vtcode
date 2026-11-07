//! Correlation of streamed tool calls with their completed-response copies.

use hashbrown::HashSet;

use crate::provider::ToolCall;

/// Locate the completed-output call that corresponds to one streamed call.
///
/// Provider-issued ids are authoritative and win over position. A fabricated
/// id exists only inside a stream decoder, so the completed response can never
/// repeat it; those calls are correlated with the first unclaimed completed
/// call of the same type and tool name. Streamed calls and completed tool
/// calls both preserve output order, and `claimed` keeps two streamed calls
/// from consuming the same completed call.
pub(crate) fn correlated_response_call(
    response_calls: &[ToolCall],
    claimed: &[bool],
    streamed_call: &ToolCall,
    fabricated_tool_call_ids: &HashSet<String>,
) -> Option<usize> {
    let fabricated = fabricated_tool_call_ids.contains(streamed_call.id.as_str());
    response_calls.iter().enumerate().find_map(|(position, call)| {
        if claimed.get(position).copied().unwrap_or(true) {
            return None;
        }
        if fabricated {
            (call.call_type == streamed_call.call_type && call.tool_name() == streamed_call.tool_name())
                .then_some(position)
        } else {
            (call.id == streamed_call.id).then_some(position)
        }
    })
}

#[cfg(test)]
mod tests {
    //! Coverage for the call-correlation predicate both Responses decoders use.
    //!
    //! The tests pin the rule that a streamed call is adopted only when exactly
    //! one completed response call matches its identifiers, so fabricated or
    //! ambiguous identifiers never select a tool-call slot.

    use super::correlated_response_call;
    use crate::provider::ToolCall;
    use hashbrown::HashSet;

    /// Collect fabricated call ids for the correlation predicate.
    fn fabricated_ids(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    /// Prove a fabricated call correlates by type and name position.
    #[test]
    fn fabricated_call_is_correlated_by_type_and_name_position() {
        let response_calls = vec![
            ToolCall::function("call_read".to_string(), "read_file".to_string(), "{}".to_string()),
            ToolCall::function("call_search".to_string(), "search_workspace".to_string(), "{}".to_string()),
        ];
        let streamed_call =
            ToolCall::function("call_generated".to_string(), "search_workspace".to_string(), "{}".to_string());

        assert_eq!(
            correlated_response_call(
                &response_calls,
                &[false, false],
                &streamed_call,
                &fabricated_ids(&["call_generated"]),
            ),
            Some(1)
        );
    }

    /// Prove provider-issued ids take precedence over name and position.
    #[test]
    fn provider_issued_ids_take_precedence_over_name_position() {
        let response_calls = vec![
            ToolCall::function("call_name_match".to_string(), "search_workspace".to_string(), "{}".to_string()),
            ToolCall::function("call_id_match".to_string(), "read_file".to_string(), "{}".to_string()),
        ];
        let streamed_call =
            ToolCall::function("call_id_match".to_string(), "search_workspace".to_string(), "{}".to_string());

        assert_eq!(
            correlated_response_call(&response_calls, &[false, false], &streamed_call, &HashSet::new()),
            Some(1)
        );
    }

    /// Prove a claimed completed call is never correlated twice.
    #[test]
    fn claimed_completed_calls_are_never_correlated_twice() {
        let response_calls = vec![ToolCall::function(
            "call_search".to_string(),
            "search_workspace".to_string(),
            "{}".to_string(),
        )];
        let streamed_call =
            ToolCall::function("call_generated".to_string(), "search_workspace".to_string(), "{}".to_string());

        assert_eq!(
            correlated_response_call(&response_calls, &[true], &streamed_call, &fabricated_ids(&["call_generated"]),),
            None
        );
    }
}
