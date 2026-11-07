use std::collections::BTreeMap;
use std::str::FromStr;

use crate::core::{CustomProviderConfig, ProviderOverrideConfig};
use crate::models::Provider;
use hashbrown::HashSet;

use super::ModelId;

impl ModelId {
    /// Return the OpenRouter vendor slug when this identifier maps to a marketplace listing
    pub fn openrouter_vendor(&self) -> Option<&'static str> {
        self.openrouter_metadata().map(|meta| meta.vendor)
    }

    /// Get all available models as a vector
    pub fn all_models() -> Vec<ModelId> {
        let mut models = vec![
            // Gemini models
            ModelId::Gemini36Flash,
            ModelId::Gemini37Flash,
            ModelId::Gemini38Flash,
            // OpenAI models
            ModelId::GPT6Astra,
            ModelId::GPT56Sol,
            ModelId::GPT56Terra,
            ModelId::GPT56Luna,
            ModelId::OpenAIGptOss20b,
            ModelId::OpenAIGptOss120b,
            // Anthropic models
            ModelId::ClaudeSonnet5,
            ModelId::ClaudeFable5,
            ModelId::ClaudeFable51,
            ModelId::ClaudeMythos5,
            ModelId::ClaudeMythos51,
            ModelId::ClaudeOpus5,
            ModelId::CopilotAuto,
            ModelId::CopilotGPT52Codex,
            ModelId::CopilotGPT51CodexMax,
            ModelId::CopilotGPT54,
            ModelId::CopilotGPT54Mini,
            ModelId::CopilotClaudeSonnet46,
            // DeepSeek models
            ModelId::DeepSeekV4Pro,
            ModelId::DeepSeekV4Flash,
            ModelId::DeepSeekV4FlashVisionExp,
            // Official Meta AI models (kept before marketplace entries)
            ModelId::MetaMuseSpark13,
            ModelId::MetaMuseSpark13Contributor,
            ModelId::MetaMuseSpark12,
            ModelId::MetaMuseSpark12Contributor,
            ModelId::MetaMuseSpark11,
            // NVIDIA NIM models
            ModelId::NvidiaNemotron3Ultra550bA55b,
            ModelId::NvidiaNemotron3Super120bA12b,
            ModelId::NvidiaNemotron3Nano30bA3b,
            ModelId::NvidiaZaiGlm52,
            ModelId::NvidiaDeepseekV4Flash0731,
            // Merge Gateway routes
            ModelId::MergeGatewayDefaultRouting,
            ModelId::MergeGatewayOpenAIGpt55,
            ModelId::MergeGatewayAnthropicClaudeOpus5,
            ModelId::MergeGatewayGoogleGemini36Flash,
            ModelId::MergeGatewayGoogleGemini37Flash,
            ModelId::MergeGatewayDeepseekV4Pro0813,
            ModelId::MergeGatewayDeepseekV4Flash0731,
            ModelId::MergeGatewayXaiGrok46,
            ModelId::MergeGatewayQwen38Max,
            ModelId::MergeGatewayMinimaxH3,
            ModelId::MergeGatewayMoonshotKimiK3,
            ModelId::MergeGatewayThinkingMachinesInkling,
            ModelId::MergeGatewayMetaMuseSpark11,
            ModelId::MergeGatewayMetaMuseSpark13,
            ModelId::MergeGatewayZaiGlm53Flash,
            ModelId::MergeGatewayOpenAIGpt56Luna,
            ModelId::MergeGatewayOpenAIGpt56Sol,
            ModelId::MergeGatewayOpenAIGpt56Terra,
            ModelId::MergeGatewayGoogleGemini38Flash,
            ModelId::MergeGatewayAnthropicClaudeFable51,
            ModelId::MergeGatewayDeepseekV4Flash0731Fast,
            // Mistral models
            ModelId::MistralLarge3,
            // Z.AI models
            ModelId::ZaiGlm53,
            ModelId::ZaiGlm53Flash,
            ModelId::ZaiGlm52,
            // MiMo models
            ModelId::MiMoV25Pro,
            ModelId::MiMoV25,
            // Moonshot models
            ModelId::MoonshotKimiK3,
            ModelId::MoonshotKimiK27Code,
            // OpenCode Zen models
            // OpenCode Go models
            ModelId::OpenCodeGoGlm53,
            ModelId::OpenCodeGoGlm52,
            ModelId::OpenCodeGoGpt56Luna,
            ModelId::OpenCodeGoKimiK3,
            ModelId::OpenCodeGoKimiK27Code,
            ModelId::OpenCodeGoMimoV25,
            ModelId::OpenCodeGoMimoV25Pro,
            ModelId::OpenCodeGoMinimaxM3,
            ModelId::OpenCodeGoMuseSpark12Contributor,
            ModelId::OpenCodeGoQwen38Max,
            ModelId::OpenCodeGoQwen37Max,
            ModelId::OpenCodeGoQwen37Plus,
            ModelId::OpenCodeGoQwen36Plus,
            ModelId::OpenCodeGoDeepseekV4Pro,
            ModelId::OpenCodeGoDeepseekV4Flash,
            ModelId::OpenCodeGoHy3,
            // Qwen models
            ModelId::QwenDeepSeekV4Flash,
            ModelId::QwenDeepSeekV4Pro,
            // Ollama models
            ModelId::OllamaGptOss20b,
            ModelId::OllamaGptOss20bCloud,
            ModelId::OllamaGptOss120bCloud,
            ModelId::OllamaDeepseekV4FlashCloud,
            ModelId::OllamaDeepseekV4ProCloud,
            ModelId::OllamaGlm52Cloud,
            ModelId::OllamaGlm53Cloud,
            ModelId::OllamaMinimaxM3Cloud,
            ModelId::OllamaKimiK27CodeCloud,
            ModelId::OllamaKimiK3Cloud,
            ModelId::OllamaGemma4,
            ModelId::OllamaLagunaXs2,
            // llama.cpp models
            ModelId::LlamaCppGemma426bA4b,
            ModelId::LlamaCppGemma4E4b,
            ModelId::LlamaCppGptOss20b,
            ModelId::LlamaCppStep35Flash,
            // MiniMax models
            ModelId::MinimaxM3,
            // Hugging Face models
            ModelId::HuggingFaceOpenAIGptOss20b,
            ModelId::HuggingFaceOpenAIGptOss120b,
            ModelId::HuggingFaceGlm52Novita,
            ModelId::HuggingFaceGlm53FlashTogether,
            ModelId::HuggingFaceGlm53Together,
            ModelId::HuggingFaceKimiK3Together,
            ModelId::HuggingFaceDeepseekV4FlashNovita,
            ModelId::HuggingFaceDeepseekV4ProTogether,
            ModelId::HuggingFaceStep35Flash,
            ModelId::HuggingFaceMinimaxM3Novita,
            ModelId::HuggingFaceDeepseekV4ProNovita,
            ModelId::StepFun37Flash,
            ModelId::EvolinkGpt52,
            ModelId::EvolinkGpt55,
            ModelId::EvolinkDeepseekV4Pro,
            ModelId::EvolinkDeepseekV4Flash,
            ModelId::EvolinkDoubaoSeed20Pro,
            ModelId::EvolinkGemini31Pro,
            ModelId::EvolinkGemini35Flash,
            ModelId::EvolinkMinimaxM3,
            ModelId::EvolinkClaudeSonnet46,
            ModelId::EvolinkClaudeOpus48,
            ModelId::EvolinkClaudeHaiku45,
            ModelId::OpenRouterMoonshotaiKimiK3,
            ModelId::OpenRouterMoonshotaiKimiK27Code,
            ModelId::OpenRouterZaiGlm52,
            ModelId::OpenRouterZaiGlm53Flash,
            // xAI models
            ModelId::XaiGrokBuild01,
            ModelId::XaiGrok46,
            ModelId::XaiGrok420Reasoning,
            // Poolside models
            ModelId::PoolsideLagunaM1,
            ModelId::PoolsideLagunaXs2,
            ModelId::PoolsideLagunaS21,
        ];
        models.extend(Self::openrouter_models());
        let mut seen = HashSet::new();
        models.retain(|model| seen.insert(model.clone()));
        models
    }

    /// Get all models for a specific provider
    pub fn models_for_provider(provider: Provider) -> Vec<ModelId> {
        Self::all_models()
            .into_iter()
            .filter(|model| model.provider() == provider)
            .collect()
    }

    /// Return all models including user-defined overrides from config.
    ///
    /// Merges the hardcoded model list with custom models defined in
    /// `[providers.<name>]` config sections. Custom models are appended
    /// as `ModelId::Custom` variants keyed by provider name.
    pub fn all_models_with_overrides(overrides: &BTreeMap<String, ProviderOverrideConfig>) -> Vec<ModelId> {
        let mut models = Self::all_models();
        for (provider_key, config) in overrides {
            for model_name in &config.models {
                let trimmed = model_name.trim().to_string();
                if !trimmed.is_empty() {
                    models.push(ModelId::Custom(provider_key.clone(), trimmed));
                }
            }
        }
        models
    }

    /// Get all models for a specific provider, including user-defined overrides.
    pub fn models_for_provider_with_overrides(
        provider: Provider,
        overrides: &BTreeMap<String, ProviderOverrideConfig>,
    ) -> Vec<ModelId> {
        Self::all_models_with_overrides(overrides)
            .into_iter()
            .filter(|model| model.provider() == provider)
            .collect()
    }

    /// Resolve a model identifier against a configuration, falling back to
    /// [`ModelId::from_str`].
    ///
    /// Models declared under the active provider in `[providers.<name>]`
    /// overrides or by a `[[custom_providers]]` profile are not part of the
    /// static catalogue, so they are represented as [`ModelId::Custom`].
    /// Matching is scoped to the active provider to avoid mis-routing a model
    /// ID shared across providers, mirroring the catalogue and custom-provider
    /// branches of the subagent resolution path. Local-provider pass-through
    /// (arbitrary Ollama/llama.cpp IDs) is intentionally not handled here.
    pub fn from_config(
        model: &str,
        provider: &str,
        provider_overrides: &BTreeMap<String, ProviderOverrideConfig>,
        custom_providers: &[CustomProviderConfig],
    ) -> Result<Self, crate::models::ModelParseError> {
        let trimmed = model.trim();
        if let Ok(parsed) = Self::from_str(trimmed) {
            return Ok(parsed);
        }
        let hinted_provider = provider.trim().parse::<Provider>().ok();
        for (provider_key, override_cfg) in provider_overrides {
            let matches_hint = match hinted_provider {
                Some(active) => provider_key.parse::<Provider>().ok() == Some(active),
                None => provider_key.eq_ignore_ascii_case(provider.trim()),
            };
            if matches_hint && override_cfg.models.iter().any(|candidate| candidate.trim() == trimmed) {
                return Ok(ModelId::Custom(provider_key.clone(), trimmed.to_owned()));
            }
        }
        for custom in custom_providers {
            if custom.name.eq_ignore_ascii_case(provider.trim())
                && custom.effective_models().iter().any(|candidate| candidate == trimmed)
            {
                return Ok(ModelId::Custom(custom.name.to_lowercase(), trimmed.to_owned()));
            }
        }
        Self::from_str(trimmed)
    }
}
