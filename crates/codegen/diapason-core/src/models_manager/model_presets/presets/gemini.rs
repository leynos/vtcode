//! gemini_presets — provider preset definitions for gemini.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn gemini_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: "gemini-3.8-flash".to_string(),
            model: "gemini-3.8-flash".to_string(),
            display_name: "Gemini 3.8 Flash".to_string(),
            description:
                "Most intelligent Flash for long-horizon SWE, agents, and enterprise workflows with 1M context"
                    .to_string(),
            provider: Provider::Gemini,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Low,
                    description: "Fast responses".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Medium,
                    description: "Balanced reasoning".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Deep reasoning".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
        ModelPreset {
            id: "gemini-3.7-flash".to_string(),
            model: "gemini-3.7-flash".to_string(),
            display_name: "Gemini 3.7 Flash".to_string(),
            description: "Flash model with 1M context and tunable thinking levels (low, medium, high)".to_string(),
            provider: Provider::Gemini,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Low,
                    description: "Fast responses".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Medium,
                    description: "Balanced reasoning".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Deep reasoning".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
        ModelPreset {
            id: "gemini-3.6-flash".to_string(),
            model: "gemini-3.6-flash".to_string(),
            display_name: "Gemini 3.6 Flash".to_string(),
            description: "Latest flash model with improved reasoning and efficiency".to_string(),
            provider: Provider::Gemini,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Low,
                    description: "Fast responses".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Medium,
                    description: "Balanced reasoning".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Deep reasoning".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
        ModelPreset {
            id: "gemini-3-flash-preview".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            display_name: "Gemini 3 Flash Preview".to_string(),
            description: "Most intelligent model built for speed with superior search and grounding".to_string(),
            provider: Provider::Gemini,
            default_reasoning_effort: ReasoningEffortLevel::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Low,
                    description: "Fast responses".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Medium,
                    description: "Balanced reasoning".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Deep reasoning".to_string(),
                },
            ],
            is_default: true,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_048_576),
        },
    ]
}
