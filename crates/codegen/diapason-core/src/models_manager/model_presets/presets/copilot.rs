//! copilot_presets — provider preset definitions for copilot.

use super::super::ModelPreset;
use crate::config::constants::models::copilot as copilot_models;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn copilot_presets() -> Vec<ModelPreset> {
    vec![ModelPreset {
        id: copilot_models::AUTO.to_string(),
        model: copilot_models::AUTO.to_string(),
        display_name: "GitHub Copilot Auto".to_string(),
        description: "Official GitHub Copilot preview provider via the Copilot CLI with automatic model selection."
            .to_string(),
        provider: Provider::Copilot,
        default_reasoning_effort: ReasoningEffortLevel::Medium,
        supported_reasoning_efforts: Vec::new(),
        is_default: true,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
        context_window: Some(400_000),
    }]
}
