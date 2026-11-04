//! moonshot_presets — provider preset definitions for moonshot.

use super::super::ModelPreset;
use super::reasoning_preset;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn moonshot_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: "kimi-k3".to_string(),
            model: "kimi-k3".to_string(),
            display_name: "Kimi K3 (Moonshot)".to_string(),
            description:
                "Moonshot's 2.8T parameter flagship with 1M context, native vision, and always-on deep reasoning."
                    .to_string(),
            provider: Provider::Moonshot,
            default_reasoning_effort: ReasoningEffortLevel::Max,
            supported_reasoning_efforts: vec![reasoning_preset(
                ReasoningEffortLevel::Max,
                "Maximum (only supported level)",
            )],
            is_default: true,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
        ModelPreset {
            id: "kimi-k2.7-code".to_string(),
            model: "kimi-k2.7-code".to_string(),
            display_name: "Kimi K2.7 Code (Moonshot)".to_string(),
            description: "Moonshot's most capable coding model with long-horizon coding breakthrough.".to_string(),
            provider: Provider::Moonshot,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![reasoning_preset(ReasoningEffortLevel::Medium, "Balanced")],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(256_000),
        },
    ]
}
