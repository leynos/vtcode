//! opencode_zen_presets — provider preset definitions for opencode_zen.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn opencode_zen_presets() -> Vec<ModelPreset> {
    vec![ModelPreset {
        id: "opencode/gpt-5.4".to_string(),
        model: "gpt-5.6-sol".to_string(),
        display_name: "GPT-5.4 (OpenCode Zen)".to_string(),
        description: "OpenCode Zen gateway — curated, benchmarked models at cost".to_string(),
        provider: Provider::OpenCodeZen,
        default_reasoning_effort: ReasoningEffortLevel::Medium,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffortLevel::Medium,
            description: "Balanced".to_string(),
        }],
        is_default: true,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
        context_window: Some(1_050_000),
    }]
}
