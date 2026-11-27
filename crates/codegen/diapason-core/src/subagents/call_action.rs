//! The action and targets a unified `agent` call names.
//!
//! The ACP admission layer and the core executor must agree about which task a
//! call addresses: a disagreement would admit a call for one task and then run
//! it against another. Both therefore read the call through this parser instead
//! of each inferring the legacy shape for itself.

use serde_json::Value;

use crate::config::constants::tools;

/// The action a unified `agent` call names, after legacy inference.
///
/// A call that omits `action` is inferred from its argument shape: `id` with
/// `message` or `items` is `send_input`, `id` alone is `resume`, and anything
/// else is `spawn`. That inference predates the explicit action and is kept so
/// older callers keep working.
#[must_use]
pub fn agent_call_action(args: &Value) -> String {
    args.get("action")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| {
            let has_id = args.get("id").is_some();
            let has_input = args.get("message").is_some() || args.get("items").is_some();
            match (has_id, has_input) {
                (true, true) => "send_input".to_string(),
                (true, false) => "resume".to_string(),
                _ => "spawn".to_string(),
            }
        })
}

/// The subagent ids a call of `tool_name` addresses.
///
/// A call that names no task — a spawn, or an action the caller must still
/// choose — yields no targets, so an admission check has nothing to reject.
#[must_use]
pub fn agent_call_targets(tool_name: &str, args: &Value) -> Vec<String> {
    match tool_name {
        tools::SEND_INPUT | tools::RESUME_AGENT | tools::CLOSE_AGENT => {
            args.get("id").and_then(Value::as_str).map(str::to_owned).into_iter().collect()
        }
        tools::WAIT_AGENT => string_array(args, "ids"),
        tools::AGENT => match agent_call_action(args).as_str() {
            "send_input" | "resume" | "resume_agent" | "close" => {
                args.get("id").and_then(Value::as_str).map(str::to_owned).into_iter().collect()
            }
            "wait" => string_array(args, "ids"),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_action_is_lowercased() {
        let args = serde_json::json!({ "action": "SEND_INPUT", "id": "child" });
        assert_eq!(agent_call_action(&args), "send_input");
    }

    #[test]
    fn a_legacy_call_with_id_and_message_is_send_input() {
        let args = serde_json::json!({ "id": "child", "message": "go on" });
        assert_eq!(agent_call_action(&args), "send_input");
        assert_eq!(agent_call_targets(tools::AGENT, &args), vec!["child".to_string()]);
    }

    #[test]
    fn a_legacy_call_with_only_an_id_is_resume() {
        let args = serde_json::json!({ "id": "child" });
        assert_eq!(agent_call_action(&args), "resume");
        assert_eq!(agent_call_targets(tools::AGENT, &args), vec!["child".to_string()]);
    }

    #[test]
    fn a_call_without_a_task_names_no_target() {
        let args = serde_json::json!({ "message": "delegate this" });
        assert_eq!(agent_call_action(&args), "spawn");
        assert!(agent_call_targets(tools::AGENT, &args).is_empty());
        assert!(agent_call_targets("search", &args).is_empty());
    }

    #[test]
    fn a_wait_call_names_every_id_it_lists() {
        let args = serde_json::json!({ "action": "wait", "ids": ["a", "b"] });
        assert_eq!(agent_call_targets(tools::AGENT, &args), vec!["a".to_string(), "b".to_string()]);
    }
}
