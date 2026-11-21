//! Validation rules for custom provider configuration.
use crate::types::ReasoningEffortLevel;

use super::CustomProviderConfig;

impl CustomProviderConfig {
    /// Validate that required fields are present and the name doesn't collide
    /// with built-in provider keys.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_identity()?;
        self.validate_provider_bounds()?;
        self.validate_auth()?;
        self.validate_api_key_env()?;
        self.validate_models()?;
        self.request_policy.validate(&self.name)?;
        self.validate_profiles()?;
        self.validate_name_is_not_reserved()
    }

    fn validate_identity(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("custom_providers: `name` must not be empty".to_string());
        }

        if !is_valid_provider_name(&self.name) {
            return Err(format!(
                "custom_providers[{}]: `name` must use lowercase letters, digits, hyphens, or underscores",
                self.name
            ));
        }

        if self.display_name.trim().is_empty() {
            return Err(format!("custom_providers[{}]: `display_name` must not be empty", self.name));
        }

        if self.base_url.trim().is_empty() {
            return Err(format!("custom_providers[{}]: `base_url` must not be empty", self.name));
        }

        Ok(())
    }

    fn validate_provider_bounds(&self) -> Result<(), String> {
        self.validate_context_window_bound()?;
        self.validate_sampling_bounds()?;
        self.validate_output_bounds()
    }

    fn validate_context_window_bound(&self) -> Result<(), String> {
        if self.context_window == Some(0) {
            return Err(format!("custom_providers[{}]: `context_window` must be greater than 0", self.name));
        }

        Ok(())
    }

    fn validate_sampling_bounds(&self) -> Result<(), String> {
        for bound in [
            SamplingBound {
                field: "`temperature`",
                value: self.temperature,
                minimum: 0.0,
                maximum: 2.0,
            },
            SamplingBound {
                field: "`top_p`",
                value: self.top_p,
                minimum: 0.0,
                maximum: 1.0,
            },
        ] {
            bound.validate(&self.name)?;
        }

        if let Some(top_k) = self.top_k
            && top_k < 0
        {
            return Err(format!("custom_providers[{}]: `top_k` must be >= 0", self.name));
        }

        for bound in [
            SamplingBound {
                field: "`presence_penalty`",
                value: self.presence_penalty,
                minimum: -2.0,
                maximum: 2.0,
            },
            SamplingBound {
                field: "`frequency_penalty`",
                value: self.frequency_penalty,
                minimum: -2.0,
                maximum: 2.0,
            },
        ] {
            bound.validate(&self.name)?;
        }

        Ok(())
    }

    fn validate_output_bounds(&self) -> Result<(), String> {
        if self.max_tokens == Some(0) {
            return Err(format!("custom_providers[{}]: `max_tokens` must be greater than 0", self.name));
        }

        if self.reasoning_effort == Some(ReasoningEffortLevel::Unknown) {
            return Err(format!(
                "custom_providers[{}]: `reasoning_effort` is not a recognized level (use none, minimal, low, medium, high, xhigh, or max)",
                self.name
            ));
        }

        Ok(())
    }

    fn validate_auth(&self) -> Result<(), String> {
        if let Some(auth) = &self.auth {
            auth.validate(&self.name)?;
            if !self.api_key_env.trim().is_empty() {
                return Err(format!("custom_providers[{}]: `auth` cannot be combined with `api_key_env`", self.name));
            }
        }

        Ok(())
    }

    fn validate_api_key_env(&self) -> Result<(), String> {
        if !self.api_key_env.trim().is_empty()
            && let Err(err) = crate::auth::CredentialIdentity::new(&self.name, &self.api_key_env)
        {
            return Err(format!("custom_providers[{}]: invalid `api_key_env`: {err}", self.name));
        }

        Ok(())
    }

    fn validate_models(&self) -> Result<(), String> {
        if self.models.iter().any(|m| m.trim().is_empty()) {
            return Err(format!("custom_providers[{}]: `models` entries must not be empty", self.name));
        }

        Ok(())
    }

    fn validate_profiles(&self) -> Result<(), String> {
        for (profile_key, profile) in &self.profiles {
            if profile_key.trim().is_empty() || profile_key.trim() != profile_key {
                return Err(format!(
                    "custom_providers[{}]: profile key `{profile_key}` must not be empty or contain surrounding whitespace",
                    self.name
                ));
            }

            profile.validate(&self.name, profile_key)?;
        }

        Ok(())
    }

    fn validate_name_is_not_reserved(&self) -> Result<(), String> {
        let reserved = [
            "openai",
            "anthropic",
            "gemini",
            "copilot",
            "deepseek",
            "meta",
            "meta-ai",
            "openrouter",
            "ollama",
            "lmstudio",
            "llamacpp",
            "moonshot",
            "zai",
            "minimax",
            "huggingface",
            "openresponses",
        ];
        let lower = self.name.to_lowercase();
        if reserved.contains(&lower.as_str()) {
            return Err(format!("custom_providers[{}]: name collides with built-in provider", self.name));
        }

        Ok(())
    }
}

struct SamplingBound {
    field: &'static str,
    value: Option<f32>,
    minimum: f32,
    maximum: f32,
}

impl SamplingBound {
    fn validate(&self, provider_name: &str) -> Result<(), String> {
        let Some(value) = self.value else {
            return Ok(());
        };
        if (self.minimum..=self.maximum).contains(&value) {
            return Ok(());
        }

        Err(format!(
            "custom_providers[{provider_name}]: {} must be between {:.1} and {:.1}",
            self.field, self.minimum, self.maximum
        ))
    }
}

fn is_valid_provider_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    let Some(last) = bytes.last() else {
        return false;
    };

    let is_valid_char = |ch: u8| matches!(ch, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_');
    let is_alphanumeric = |ch: u8| matches!(ch, b'a'..=b'z' | b'0'..=b'9');

    is_alphanumeric(*first) && is_alphanumeric(*last) && bytes.iter().copied().all(is_valid_char)
}
