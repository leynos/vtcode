//! llamacpp_presets — provider preset definitions for llamacpp.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::constants::models::llamacpp as llamacpp_models;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn llamacpp_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: format!("llamacpp/{}", llamacpp_models::GPT_OSS_20B),
            model: llamacpp_models::GPT_OSS_20B.to_string(),
            display_name: "GPT-OSS 20B (llama.cpp)".to_string(),
            description: "OpenAI's open-weight GPT-OSS 20B model served locally through llama.cpp".to_string(),
            provider: Provider::LlamaCpp,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Medium,
                description: "Balanced".to_string(),
            }],
            is_default: true,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(131_072),
        },
        ModelPreset {
            id: format!("llamacpp/{}", llamacpp_models::GEMMA_4_26B_A4B),
            model: llamacpp_models::GEMMA_4_26B_A4B.to_string(),
            display_name: "Gemma 4 26B A4B (llama.cpp)".to_string(),
            description: "Gemma 4 desktop MoE model served through llama.cpp".to_string(),
            provider: Provider::LlamaCpp,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Medium,
                description: "Balanced".to_string(),
            }],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(262_144),
        },
        ModelPreset {
            id: format!("llamacpp/{}", llamacpp_models::GEMMA_4_E4B),
            model: llamacpp_models::GEMMA_4_E4B.to_string(),
            display_name: "Gemma 4 E4B (llama.cpp)".to_string(),
            description: "Tiny-footprint Gemma 4 model served through llama.cpp".to_string(),
            provider: Provider::LlamaCpp,
            default_reasoning_effort: ReasoningEffortLevel::Low,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Low,
                description: "Fast".to_string(),
            }],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(131_072),
        },
        ModelPreset {
            id: format!("llamacpp/{}", llamacpp_models::STEP_3_5_FLASH),
            model: llamacpp_models::STEP_3_5_FLASH.to_string(),
            display_name: "Step 3.5 Flash (llama.cpp)".to_string(),
            description: "StepFun's efficient reasoning model served through llama.cpp".to_string(),
            provider: Provider::LlamaCpp,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![ReasoningEffortPreset {
                effort: ReasoningEffortLevel::Medium,
                description: "Balanced".to_string(),
            }],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(262_144),
        },
    ]
}
