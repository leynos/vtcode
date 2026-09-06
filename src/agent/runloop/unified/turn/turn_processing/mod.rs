mod llm_request;
mod planning_workflow;
mod recovery_guidance;
mod response_processing;
mod result_handler;
#[cfg(test)]
pub(crate) mod test_support;

use vtcode_core::ActivePrimaryAgent;

pub(crate) use llm_request::execute_llm_request;
pub(crate) use llm_request::is_unmatched_tool_result_error;
pub(crate) use planning_workflow::{maybe_force_planning_workflow_interview, planning_workflow_interview_ready};
pub(crate) use response_processing::process_llm_response;
pub(crate) use result_handler::{HandleTurnProcessingResultParams, handle_turn_processing_result};

/// Resolve the model used by the request after applying the active primary
/// agent's optional model override. Compaction and request assembly must use
/// the same model so provider-specific context limits cannot diverge.
pub(crate) fn resolve_effective_request_model(base_model: &str, active_primary_agent: &ActivePrimaryAgent) -> String {
    active_primary_agent
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("inherit"))
        .unwrap_or(base_model)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::resolve_effective_request_model;
    use vtcode_core::primary_agent::ActivePrimaryAgentState;

    #[test]
    fn effective_request_model_honours_agent_override_and_inherit_sentinel() {
        let mut agent = ActivePrimaryAgentState::default().active().clone();
        agent.model = Some("narrow-model".to_string());
        assert_eq!(resolve_effective_request_model("base-model", &agent), "narrow-model");

        agent.model = Some(" inherit ".to_string());
        assert_eq!(resolve_effective_request_model("base-model", &agent), "base-model");
    }
}
