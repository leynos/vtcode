//! qwen_presets — provider preset definitions for qwen.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::constants::models::qwen as qwen_models;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn qwen_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: qwen_models::DEEPSEEK_V4_FLASH.to_string(),
            model: qwen_models::DEEPSEEK_V4_FLASH.to_string(),
            display_name: "DeepSeek V4 Flash (Qwen)".to_string(),
            description: "DeepSeek V4 Flash fast inference model served through Qwen Cloud API (1M context)"
                .to_string(),
            provider: Provider::Qwen,
            default_reasoning_effort: ReasoningEffortLevel::Low,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Low,
                description: "Fast".to_string(),
            }],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
        ModelPreset {
            id: qwen_models::DEEPSEEK_V4_PRO.to_string(),
            model: qwen_models::DEEPSEEK_V4_PRO.to_string(),
            display_name: "DeepSeek V4 Pro (Qwen)".to_string(),
            description: "DeepSeek V4 Pro high-performance reasoning model served through Qwen Cloud API (1M context)"
                .to_string(),
            provider: Provider::Qwen,
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
