//! mimo_presets — provider preset definitions for mimo.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::constants::models::mimo as mimo_models;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn mimo_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: mimo_models::MIMO_V2_5_PRO.to_string(),
            model: mimo_models::MIMO_V2_5_PRO.to_string(),
            display_name: "MiMo V2.5 Pro".to_string(),
            description: "Xiaomi's flagship reasoning model with advanced capabilities (1M context)".to_string(),
            provider: Provider::MiMo,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Medium,
                description: "Balanced".to_string(),
            }],
            is_default: true,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
        ModelPreset {
            id: mimo_models::MIMO_V2_5.to_string(),
            model: mimo_models::MIMO_V2_5.to_string(),
            display_name: "MiMo V2.5".to_string(),
            description: "Xiaomi's general-purpose model with strong reasoning (1M context)".to_string(),
            provider: Provider::MiMo,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Medium,
                description: "Balanced".to_string(),
            }],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
    ]
}
