use super::super::{ModelPreset, ReasoningEffortPreset};
use super::reasoning_preset;
use crate::config::constants::models;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;

fn reasoning_efforts() -> Vec<ReasoningEffortPreset> {
    vec![
        reasoning_preset(ReasoningEffortLevel::Low, "Fast thinking"),
        reasoning_preset(ReasoningEffortLevel::Medium, "Balanced thinking"),
        reasoning_preset(ReasoningEffortLevel::High, "Deep thinking"),
    ]
}

pub(crate) fn nvidia_presets() -> Vec<ModelPreset> {
    [
        (
            models::nvidia::NEMOTRON_3_ULTRA_550B_A55B,
            "Nemotron 3 Ultra (NVIDIA)",
            "NVIDIA's flagship Nemotron model for long-context agentic reasoning, coding, planning, and tool use",
            true,
        ),
        (
            models::nvidia::NEMOTRON_3_SUPER_120B_A12B,
            "Nemotron 3 Super (NVIDIA)",
            "Efficient long-context reasoning and tool use through NVIDIA NIM",
            false,
        ),
        (
            models::nvidia::NEMOTRON_3_NANO_30B_A3B,
            "Nemotron 3 Nano (NVIDIA)",
            "Efficient NVIDIA model for coding, reasoning, instruction following, and tool use",
            false,
        ),
        (
            models::nvidia::Z_AI_GLM_5_2,
            "GLM-5.2 (NVIDIA)",
            "Z.AI GLM-5.2 served through NVIDIA NIM for long-horizon reasoning and coding",
            false,
        ),
        (
            models::nvidia::DEEPSEEK_V4_FLASH_0731,
            "DeepSeek V4 Flash (NVIDIA)",
            "DeepSeek V4 Flash served through NVIDIA NIM for fast reasoning and agentic coding",
            false,
        ),
    ]
    .into_iter()
    .map(|(model, display_name, description, is_default)| ModelPreset {
        id: model.to_string(),
        model: model.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        provider: Provider::NVIDIA,
        default_reasoning_effort: ReasoningEffortLevel::High,
        supported_reasoning_efforts: reasoning_efforts(),
        is_default,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
        context_window: Some(1_000_000),
    })
    .collect()
}
