//! Per-model pricing for a custom provider.
//!
//! Configuration states rates in USD per million tokens; the runtime cost
//! estimator wants per-token rates, so this module owns the conversion, the
//! layer inheritance, and the validation that keeps a partial or impossible
//! price out of a published usage update.

use serde::{Deserialize, Serialize};

use crate::models::ModelPricing;

/// Optional per-token pricing for a custom provider model.
///
/// Values are configured in USD per million tokens and converted to the
/// per-token representation used by the runtime cost estimator.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
pub struct CustomProviderPricingConfig {
    /// USD charged per million input tokens. `costUSD` is omitted from ACP
    /// usage updates unless both this and the output rate are configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million_usd: Option<f64>,

    /// USD charged per million output tokens. Required alongside the input
    /// rate before usage updates carry `costUSD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million_usd: Option<f64>,

    /// USD charged per million cache-read tokens. Used only when the provider
    /// reports cache-read token counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_million_usd: Option<f64>,

    /// USD charged per million cache-write tokens. Used only when the provider
    /// reports cache-creation token counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_million_usd: Option<f64>,
}

impl CustomProviderPricingConfig {
    const TOKENS_PER_MILLION: f64 = 1_000_000.0;

    /// Overlay one layer's pricing onto another's, field by field.
    ///
    /// A rate set on the overlay wins; an unset rate inherits the base value.
    pub(super) fn from_layers(defaults: Self, profile: Self) -> Self {
        Self {
            input_per_million_usd: profile.input_per_million_usd.or(defaults.input_per_million_usd),
            output_per_million_usd: profile.output_per_million_usd.or(defaults.output_per_million_usd),
            cache_read_per_million_usd: profile.cache_read_per_million_usd.or(defaults.cache_read_per_million_usd),
            cache_write_per_million_usd: profile.cache_write_per_million_usd.or(defaults.cache_write_per_million_usd),
        }
    }

    /// Convert the configured per-million rates to per-token pricing.
    ///
    /// Returns `None` until both input and output rates are configured, since
    /// a partial price cannot describe a complete model cost.
    pub fn model_pricing(self) -> Option<ModelPricing> {
        let input = self.input_per_million_usd? / Self::TOKENS_PER_MILLION;
        let output = self.output_per_million_usd? / Self::TOKENS_PER_MILLION;
        Some(ModelPricing {
            input: Some(input),
            output: Some(output),
            cache_read: self.cache_read_per_million_usd.map(|rate| rate / Self::TOKENS_PER_MILLION),
            cache_write: self.cache_write_per_million_usd.map(|rate| rate / Self::TOKENS_PER_MILLION),
        })
    }

    /// Reject rates that are negative or not finite.
    ///
    /// `subject` prefixes the error so the report names the configuration
    /// layer the invalid rate came from.
    pub(super) fn validate(self, subject: &str) -> Result<(), String> {
        let rates = [
            ("input_per_million_usd", self.input_per_million_usd),
            ("output_per_million_usd", self.output_per_million_usd),
            ("cache_read_per_million_usd", self.cache_read_per_million_usd),
            ("cache_write_per_million_usd", self.cache_write_per_million_usd),
        ];
        for (name, rate) in rates {
            if rate.is_some_and(|rate| !rate.is_finite() || rate < 0.0) {
                return Err(format!("{subject}.pricing: `{name}` must be a finite non-negative number"));
            }
        }
        Ok(())
    }

    /// Report whether no rate is configured at all.
    pub(super) fn is_empty(&self) -> bool {
        self.input_per_million_usd.is_none()
            && self.output_per_million_usd.is_none()
            && self.cache_read_per_million_usd.is_none()
            && self.cache_write_per_million_usd.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::CustomProviderPricingConfig;

    /// Assert per-million pricing converts to per-token rates.
    #[test]
    fn custom_provider_pricing_converts_per_million_rates_to_per_token_rates() {
        let pricing = CustomProviderPricingConfig {
            input_per_million_usd: Some(0.13),
            output_per_million_usd: Some(0.26),
            cache_read_per_million_usd: Some(0.028),
            cache_write_per_million_usd: None,
        }
        .model_pricing()
        .expect("complete custom pricing");

        assert_eq!(pricing.input, Some(0.13 / 1_000_000.0));
        assert_eq!(pricing.output, Some(0.26 / 1_000_000.0));
        assert_eq!(pricing.cache_read, Some(0.028 / 1_000_000.0));
        assert_eq!(pricing.cache_write, None);
    }

    /// Assert negative or non-finite rates are rejected.
    #[test]
    fn custom_provider_pricing_rejects_negative_or_non_finite_rates() {
        for invalid_rate in [-0.01, f64::INFINITY, f64::NAN] {
            let pricing = CustomProviderPricingConfig {
                input_per_million_usd: Some(invalid_rate),
                ..Default::default()
            };
            assert!(pricing.validate("custom_providers[test]").is_err());
        }
    }
}
