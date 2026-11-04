//! Provider context-management / compaction plumbing.
//!
//! Builds the `context_management` payload sent on the wire: Anthropic's
//! native `context_management.edits` (tool-result clearing plus the
//! `compact_*` edit) when the active provider is Anthropic, or the
//! server-compaction hint payload for providers that support responses
//! compaction. Invariant: only one shape is ever returned for a given turn
//! (Anthropic edits XOR server-compaction hint), selected purely from
//! [`TurnRequestSnapshot::provider_name`] and provider capabilities -- never
//! both.

use std::str::FromStr;

use diapason_config::context::default_max_context_tokens;
use diapason_core::core::agent::features::FeatureSet;
use diapason_core::tools::handlers::anthropic_native_memory_enabled_for_runtime;

use crate::agent::runloop::unified::turn::compaction::{
    build_server_compaction_context_management, resolve_effective_compaction_threshold,
};
use crate::agent::runloop::unified::turn::context::TurnProcessingContext;

use super::snapshot::TurnRequestSnapshot;

pub(super) fn resolve_context_management(
    ctx: &TurnProcessingContext<'_>,
    turn: &TurnRequestSnapshot,
    active_model: &str,
) -> Option<serde_json::Value> {
    let Some(vt_cfg) = ctx.vt_cfg else {
        return resolve_server_compaction_context_management(turn, None, None);
    };

    if turn.provider_name.eq_ignore_ascii_case("anthropic") {
        return build_anthropic_context_management(vt_cfg, turn, active_model);
    }

    resolve_server_compaction_context_management(
        turn,
        Some(vt_cfg),
        vt_cfg.agent.harness.auto_compaction_threshold_tokens,
    )
}

fn resolve_server_compaction_context_management(
    turn: &TurnRequestSnapshot,
    vt_cfg: Option<&diapason_core::config::loader::DiapasonConfig>,
    configured_threshold: Option<u64>,
) -> Option<serde_json::Value> {
    let features = FeatureSet::from_config(vt_cfg);
    if !features.auto_compaction_enabled(turn.capabilities.responses_compaction) {
        return None;
    }

    build_server_compaction_context_management(
        configured_threshold,
        turn.context_window_size,
        vt_cfg.map_or_else(default_max_context_tokens, |cfg| cfg.context.max_context_tokens),
    )
}

fn build_anthropic_context_management(
    vt_cfg: &diapason_core::config::loader::DiapasonConfig,
    turn: &TurnRequestSnapshot,
    active_model: &str,
) -> Option<serde_json::Value> {
    if !turn.capabilities.context_edits {
        return None;
    }

    let mut edits = Vec::new();
    let clearing = &vt_cfg.agent.harness.tool_result_clearing;
    if clearing.enabled {
        let mut edit = serde_json::Map::from_iter([
            ("type".to_string(), serde_json::Value::String("clear_tool_uses_20250919".to_string())),
            (
                "trigger".to_string(),
                serde_json::json!({
                    "type": "input_tokens",
                    "value": clearing.trigger_tokens,
                }),
            ),
            (
                "keep".to_string(),
                serde_json::json!({
                    "type": "tool_uses",
                    "value": clearing.keep_tool_uses,
                }),
            ),
            (
                "clear_at_least".to_string(),
                serde_json::json!({
                    "type": "input_tokens",
                    "value": clearing.clear_at_least_tokens,
                }),
            ),
            ("clear_tool_inputs".to_string(), serde_json::Value::Bool(clearing.clear_tool_inputs)),
        ]);

        if anthropic_native_memory_enabled_for_runtime(
            diapason_core::config::models::Provider::from_str(&turn.provider_name).ok(),
            active_model,
            Some(vt_cfg),
        ) {
            edit.insert(
                "exclude_tools".to_string(),
                serde_json::json!([diapason_core::config::constants::tools::MEMORY]),
            );
        }

        edits.push(serde_json::Value::Object(edit));
    }

    if vt_cfg.agent.harness.auto_compaction_enabled
        && let Some(trigger_tokens) = resolve_effective_compaction_threshold(
            vt_cfg.agent.harness.auto_compaction_threshold_tokens,
            turn.context_window_size,
            vt_cfg.context.max_context_tokens,
        )
    {
        let mut compact_edit = serde_json::Map::new();
        compact_edit.insert("type".to_string(), serde_json::Value::String("compact_20260112".to_string()));
        compact_edit.insert(
            "trigger".to_string(),
            serde_json::json!({
                "type": "input_tokens",
                "value": trigger_tokens,
            }),
        );

        if let Some(instructions) = &vt_cfg.agent.harness.auto_compaction_instructions {
            compact_edit.insert("instructions".to_string(), serde_json::Value::String(instructions.clone()));
        }

        if vt_cfg.agent.harness.auto_compaction_pause_after {
            compact_edit.insert("pause_after_compaction".to_string(), serde_json::Value::Bool(true));
        }

        edits.push(serde_json::Value::Object(compact_edit));
    }

    (!edits.is_empty()).then(|| {
        serde_json::json!({
            "edits": edits,
        })
    })
}

#[cfg(test)]
mod tests {
    use diapason_core::config::loader::DiapasonConfig;
    use serde_json::json;

    use crate::agent::runloop::unified::turn::compaction::build_server_compaction_context_management;

    #[test]
    fn server_supported_request_build_keeps_context_management_payload() {
        let mut cfg = DiapasonConfig::default();
        cfg.agent.harness.auto_compaction_enabled = true;
        cfg.agent.harness.auto_compaction_threshold_tokens = Some(512);

        let payload = build_server_compaction_context_management(
            cfg.agent.harness.auto_compaction_threshold_tokens,
            2_000,
            cfg.context.max_context_tokens,
        );

        assert_eq!(
            payload,
            Some(json!([{
                "type": "compaction",
                "compact_threshold": 512,
            }]))
        );
    }

    #[test]
    fn server_supported_request_build_uses_session_safety_budget_by_default() {
        assert_eq!(
            build_server_compaction_context_management(None, 500_000, 160_000),
            Some(json!([{
                "type": "compaction",
                "compact_threshold": 144000,
            }]))
        );
        assert_eq!(
            build_server_compaction_context_management(None, 100_000, 160_000),
            Some(json!([{
                "type": "compaction",
                "compact_threshold": 90000,
            }]))
        );
    }
}
