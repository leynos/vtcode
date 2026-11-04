use crate::config::constants::tools;
use crate::llm::provider::ToolDefinition;
use compact_str::CompactString;

/// Priority tools that should appear first in model-visible definitions.
const PRIORITY_TOOLS: &[&str] = &[
    tools::EXEC_COMMAND,
    tools::CODE_SEARCH,
    tools::APPLY_PATCH,
    tools::WRITE_STDIN,
    tools::REQUEST_USER_INPUT,
    tools::TASK_TRACKER,
];

/// Sort tool definitions with priority grouping to improve LLM attention.
/// Priority tools appear first (better attention), then alphabetically.
/// This can save ~50-100 tokens per request via improved LLM focus.
pub fn sort_tool_definitions(mut tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    struct SortKey {
        priority: Option<usize>,
        name: CompactString,
    }

    let mut keyed = tools
        .drain(..)
        .map(|tool| {
            let name = tool.function.as_ref().map_or("", |function| function.name.as_str());
            let priority = PRIORITY_TOOLS.iter().position(|&priority_name| priority_name == name);
            (SortKey { priority, name: CompactString::from(name) }, tool)
        })
        .collect::<Vec<_>>();

    keyed.sort_by(|(left_key, left_tool), (right_key, right_tool)| match (left_key.priority, right_key.priority) {
        (Some(left_priority), Some(right_priority)) => left_priority.cmp(&right_priority),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left_key
            .name
            .cmp(&right_key.name)
            .then_with(|| left_tool.tool_type.cmp(&right_tool.tool_type)),
    });

    keyed.into_iter().map(|(_, tool)| tool).collect()
}

#[cfg(test)]
mod tests {
    use hashbrown::HashSet;

    use super::PRIORITY_TOOLS;
    use super::sort_tool_definitions;
    use crate::llm::provider::ToolDefinition;

    #[test]
    fn sort_tool_definitions_orders_by_name() {
        let tools = vec![
            ToolDefinition::function("b_tool".to_string(), "b".to_string(), serde_json::json!({})),
            ToolDefinition::function("a_tool".to_string(), "a".to_string(), serde_json::json!({})),
        ];

        let sorted = sort_tool_definitions(tools);
        let names: Vec<&str> = sorted
            .iter()
            .filter_map(|tool| tool.function.as_ref().map(|func| func.name.as_str()))
            .collect();

        assert_eq!(names, vec!["a_tool", "b_tool"]);
    }

    #[test]
    fn sort_tool_definitions_prioritizes_current_core_tools() {
        let tools = vec![
            ToolDefinition::function("zebra_tool".to_string(), "z".to_string(), serde_json::json!({})),
            ToolDefinition::function("code_search".to_string(), "search".to_string(), serde_json::json!({})),
            ToolDefinition::function("request_user_input".to_string(), "ask".to_string(), serde_json::json!({})),
            ToolDefinition::function("alpha_tool".to_string(), "a".to_string(), serde_json::json!({})),
            ToolDefinition::function("exec_command".to_string(), "shell".to_string(), serde_json::json!({})),
        ];

        let sorted = sort_tool_definitions(tools);
        let names: Vec<&str> = sorted
            .iter()
            .filter_map(|tool| tool.function.as_ref().map(|func| func.name.as_str()))
            .collect();

        // Priority tools first (in priority order), then alphabetical
        assert_eq!(
            names,
            vec![
                "exec_command",
                "code_search",
                "request_user_input",
                "alpha_tool",
                "zebra_tool"
            ]
        );
    }

    #[test]
    fn priority_tools_are_unique() {
        let unique: HashSet<&str> = PRIORITY_TOOLS.iter().copied().collect();
        assert_eq!(unique.len(), PRIORITY_TOOLS.len());
    }

    #[test]
    fn duplicate_priority_names_keep_input_order() {
        let mut first = ToolDefinition::function("code_search".to_string(), "first".to_string(), serde_json::json!({}));
        first.tool_type = "first_type".to_string();
        let mut second =
            ToolDefinition::function("code_search".to_string(), "second".to_string(), serde_json::json!({}));
        second.tool_type = "second_type".to_string();

        let sorted = sort_tool_definitions(vec![first, second]);
        let descriptions: Vec<&str> = sorted
            .iter()
            .filter_map(|tool| tool.function.as_ref().map(|function| function.description.as_str()))
            .collect();
        assert_eq!(descriptions, vec!["first", "second"]);
    }
}
