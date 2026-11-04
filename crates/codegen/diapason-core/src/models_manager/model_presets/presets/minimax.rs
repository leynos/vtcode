//! minimax_presets — provider preset definitions for minimax.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn minimax_presets() -> Vec<ModelPreset> {
    vec![ModelPreset {
        id: "minimax-m3".to_string(),
        model: "MiniMax-M3".to_string(),
        display_name: "MiniMax M3".to_string(),
        description: "Frontier multimodal coding model with 1M context".to_string(),
        provider: Provider::Minimax,
        default_reasoning_effort: ReasoningEffortLevel::Medium,
        supported_reasoning_efforts: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Medium,
                description: "Balanced".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffortLevel::High,
                description: "Deep".to_string(),
            },
        ],
        is_default: true,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
        context_window: Some(1_000_000),
    }]
}
