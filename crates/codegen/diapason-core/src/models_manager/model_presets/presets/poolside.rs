//! poolside_presets — provider preset definitions for poolside.

use super::super::{ModelPreset, ModelUpgrade};
use crate::config::constants::models::poolside as poolside_models;
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn poolside_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: poolside_models::LAGUNA_S_2_1.to_string(),
            model: poolside_models::LAGUNA_S_2_1.to_string(),
            display_name: "Laguna S 2.1".to_string(),
            description:
                "Poolside's 118B MoE coding agent model with 1M context, optimized for long-horizon agentic tasks, tool use, and validation"
                    .to_string(),
            provider: Provider::Poolside,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: Vec::new(),
            is_default: true,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_000_000),
        },
        ModelPreset {
            id: poolside_models::LAGUNA_M1.to_string(),
            model: poolside_models::LAGUNA_M1.to_string(),
            display_name: "Laguna M.1".to_string(),
            description:
                "Poolside's flagship MoE coding agent model optimized for multi-step agentic tasks, tool use, and validation (128K context)"
                    .to_string(),
            provider: Provider::Poolside,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: Vec::new(),
            is_default: false,
            upgrade: Some(ModelUpgrade {
                id: poolside_models::LAGUNA_S_2_1.to_string(),
                migration_config_key: String::new(),
                upgrade_copy: Some("Upgrade to Laguna S 2.1 — Poolside's most capable coding agent model with 1M context".to_string()),
                reasoning_effort_mapping: None,
                model_link: None,
            }),
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(131_072),
        },
        ModelPreset {
            id: poolside_models::LAGUNA_XS2.to_string(),
            model: poolside_models::LAGUNA_XS2.to_string(),
            display_name: "Laguna XS.2".to_string(),
            description:
                "Poolside's efficient MoE coding agent model optimized for fast agentic coding (128K context)"
                    .to_string(),
            provider: Provider::Poolside,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: Vec::new(),
            is_default: false,
            upgrade: Some(ModelUpgrade {
                id: poolside_models::LAGUNA_S_2_1.to_string(),
                migration_config_key: String::new(),
                upgrade_copy: Some("Upgrade to Laguna S 2.1 — Poolside's most capable coding agent model with 1M context".to_string()),
                reasoning_effort_mapping: None,
                model_link: None,
            }),
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(131_072),
        },
    ]
}
