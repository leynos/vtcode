//! huggingface_presets — provider preset definitions for huggingface.

use super::super::{ModelPreset, ReasoningEffortPreset};
use crate::config::models::Provider;
use crate::config::types::ReasoningEffortLevel;
pub(crate) fn huggingface_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            id: "huggingface/deepseek-v4-flash".to_string(),
            model: "deepseek-ai/DeepSeek-V4-Flash:novita".to_string(),
            display_name: "DeepSeek V4 Flash (HF/Novita)".to_string(),
            description: "Fast inference model for cost-effective reasoning (1M context, 158B params)".to_string(),
            provider: Provider::HuggingFace,
            default_reasoning_effort: ReasoningEffortLevel::High,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Balanced".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Max,
                    description: "Maximum thinking".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_000_000),
        },
        ModelPreset {
            id: "huggingface/deepseek-v4-pro".to_string(),
            model: "deepseek-ai/DeepSeek-V4-Pro:together".to_string(),
            display_name: "DeepSeek V4 Pro (HF/Together)".to_string(),
            description:
                "High-performance reasoning model with advanced thinking capabilities (1M context, 1.6T params)"
                    .to_string(),
            provider: Provider::HuggingFace,
            default_reasoning_effort: ReasoningEffortLevel::High,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Balanced".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Max,
                    description: "Maximum thinking".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_000_000),
        },
        ModelPreset {
            id: "huggingface/nvidia-nemotron-3-ultra".to_string(),
            model: "nvidia/NVIDIA-Nemotron-3-Ultra-550B-A55B-NVFP4:together".to_string(),
            display_name: "NVIDIA-Nemotron-3-Ultra-550B (HF/Together)".to_string(),
            description: "NVIDIA Nemotron 3 Ultra 550B-A55B-NVFP4 via Together inference provider".to_string(),
            provider: Provider::HuggingFace,
            default_reasoning_effort: ReasoningEffortLevel::High,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Balanced".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Max,
                    description: "Maximum thinking".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(128_000),
        },
        ModelPreset {
            id: "huggingface/minimax-m3".to_string(),
            model: "MiniMaxAI/MiniMax-M3:novita".to_string(),
            display_name: "MiniMax-M3 (HF/Novita)".to_string(),
            description: "Frontier multimodal coding model with 1M context window via Novita inference provider"
                .to_string(),
            provider: Provider::HuggingFace,
            default_reasoning_effort: ReasoningEffortLevel::High,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Balanced".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Max,
                    description: "Maximum thinking".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_000_000),
        },
        ModelPreset {
            id: "huggingface/kimi-k3".to_string(),
            model: "moonshotai/Kimi-K3:together".to_string(),
            display_name: "Kimi K3 (HF/Together)".to_string(),
            description: "Kimi K3 2.8T flagship with 1M context and native vision via Together inference provider on HuggingFace router"
                .to_string(),
            provider: Provider::HuggingFace,
            default_reasoning_effort: ReasoningEffortLevel::High,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Balanced".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Max,
                    description: "Maximum thinking".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_000_000),
        },
        ModelPreset {
            id: "huggingface/glm-5.3".to_string(),
            model: "zai-org/GLM-5.3:together".to_string(),
            display_name: "GLM-5.3 (HF/Together)".to_string(),
            description:
                "Z.ai GLM-5.3 via Together inference provider on HuggingFace router. Frontier coding model with 1M context."
                    .to_string(),
            provider: Provider::HuggingFace,
            default_reasoning_effort: ReasoningEffortLevel::High,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::High,
                    description: "Balanced".to_string(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffortLevel::Max,
                    description: "Maximum thinking".to_string(),
                },
            ],
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            context_window: Some(1_000_000),
        },
    ]
}
