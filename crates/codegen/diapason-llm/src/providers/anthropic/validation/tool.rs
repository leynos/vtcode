//! Tool definition validation for the Anthropic Claude API.

use crate::error_display;
use crate::provider::{LLMError, LLMRequest, ToolChoice};

pub(super) fn validate_tool_definitions(request: &LLMRequest) -> Result<(), LLMError> {
    let Some(tools) = request.tools.as_ref() else {
        return Ok(());
    };

    let mut has_programmatic_tool_calling = false;

    for tool in tools.iter() {
        let has_allowed_callers = tool.allowed_callers.as_ref().is_some_and(|callers| !callers.is_empty());
        let has_input_examples = tool.input_examples.as_ref().is_some_and(|examples| !examples.is_empty());

        if let Some(function) = tool.function.as_ref() {
            validate_anthropic_tool_name(&function.name)?;

            if has_allowed_callers && tool.strict == Some(true) {
                let formatted_error = error_display::format_llm_error(
                    "Anthropic",
                    &format!(
                        "tool '{}' cannot combine strict=true with allowed_callers; strict tool use is incompatible with programmatic tool calling",
                        function.name
                    ),
                );
                return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
            }
        } else if has_allowed_callers || has_input_examples {
            let formatted_error = error_display::format_llm_error(
                "Anthropic",
                &format!(
                    "tool type '{}' cannot use allowed_callers or input_examples without a function definition",
                    tool.tool_type
                ),
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        has_programmatic_tool_calling |= has_allowed_callers;
    }

    if has_programmatic_tool_calling
        && request
            .parallel_tool_config
            .as_ref()
            .is_some_and(|config| config.disable_parallel_tool_use)
    {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            "programmatic tool calling is incompatible with disable_parallel_tool_use=true",
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if has_programmatic_tool_calling && matches!(request.tool_choice, Some(ToolChoice::Any | ToolChoice::Specific(_))) {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            "programmatic tool calling is incompatible with forced tool_choice values; use 'auto' or omit tool_choice",
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    Ok(())
}

fn validate_anthropic_tool_name(name: &str) -> Result<(), LLMError> {
    let is_valid = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'));

    if is_valid {
        return Ok(());
    }

    let formatted_error = error_display::format_llm_error(
        "Anthropic",
        &format!("tool name '{name}' must match ^[a-zA-Z0-9_-]{{1,64}}$ for Anthropic tool use"),
    );
    Err(LLMError::InvalidRequest { message: formatted_error, metadata: None })
}
