//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.

use crate::acp;
#[cfg(test)]
use crate::zed::connection::ConnectionHandle;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use vtcode_config::models::ModelPricing;
use vtcode_config::{CustomProviderApiFormat, CustomProviderConfig};
use vtcode_core::llm::Usage;
use vtcode_core::llm::model_resolver::ModelResolver;
use vtcode_core::llm::usage_cost::{
    estimate_session_costs_with_pricing, normalized_turn_usage_with_input_semantics, provider_reports_exclusive_input,
};

use super::{ZedAgent, lody::lody_capabilities_mut};

const LODY_EXTENSION_VERSION: u8 = 1;
pub(super) const LODY_SESSION_USAGE_UPDATE_METHOD: &str = "_lody/session/usage_update";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LodyUsage {
    input_tokens: u32,
    output_tokens: u32,
    cache_read_input_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_output_tokens: Option<u32>,
    #[serde(rename = "costUSD", skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UsageProfile {
    context_window: Option<usize>,
    pricing: Option<ModelPricing>,
    /// Whether `prompt_tokens` excludes cached prompt tokens, so cost
    /// normalization must add them back.
    exclusive_input: bool,
}

/// Select the input-token semantics of the backend a request was actually
/// routed to.
///
/// A custom provider's name says nothing about its wire format: one named
/// `baseten` may speak Anthropic Messages, where `input_tokens` excludes cache
/// reads, while the same name may speak an OpenAI format where it includes
/// them. `api_format` is the resolved backend, so it wins over the name.
fn exclusive_input_semantics(api_format: Option<CustomProviderApiFormat>, provider: &str) -> bool {
    match api_format {
        Some(CustomProviderApiFormat::AnthropicMessages) => true,
        Some(CustomProviderApiFormat::OpenAIChat | CustomProviderApiFormat::OpenAIResponses) => false,
        Some(CustomProviderApiFormat::Auto) | None => provider_reports_exclusive_input(provider),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LodySessionUsageUpdate<'a> {
    session_id: &'a str,
    usage: LodyUsage,
    model_usage: BTreeMap<&'a str, LodyUsage>,
}

pub(super) fn add_lody_usage_capability(capabilities: &mut acp::AgentCapabilities) {
    let usage = serde_json::json!({ "version": LODY_EXTENSION_VERSION });
    if let Some(lody) = lody_capabilities_mut(capabilities) {
        let _ = lody.insert("usage".to_string(), usage);
    }
}

/// Build the `_lody/session/usage_update` notification for one turn's usage.
///
/// Reasoning tokens are reported separately and subtracted from the output
/// count, so the two fields describe disjoint token sets. Cost is omitted when
/// the provider has no pricing, and reported in the provider's input-token
/// semantics rather than reinterpreting a shared total.
fn usage_notification(
    session_id: &acp::SessionId,
    model: &str,
    usage: &Usage,
    profile: UsageProfile,
) -> anyhow::Result<acp::ExtNotification> {
    let reasoning_output_tokens = usage
        .reasoning_output_tokens
        .map(|reasoning| reasoning.min(usage.completion_tokens));
    let output_tokens = usage
        .completion_tokens
        .saturating_sub(reasoning_output_tokens.unwrap_or_default());
    let cost_usd = profile.pricing.and_then(|pricing| {
        let normalized = normalized_turn_usage_with_input_semantics(profile.exclusive_input, usage);
        estimate_session_costs_with_pricing(pricing, &normalized).map(|estimate| estimate.effective_usd)
    });
    let delta = LodyUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens,
        cache_read_input_tokens: usage.cache_read_tokens_or_fallback(),
        cache_creation_input_tokens: usage.cache_creation_tokens,
        reasoning_output_tokens,
        cost_usd,
        context_window: profile.context_window,
    };
    let payload = LodySessionUsageUpdate {
        session_id: session_id.0.as_ref(),
        usage: delta.clone(),
        model_usage: BTreeMap::from([(model, delta)]),
    };
    let params = serde_json::value::to_raw_value(&payload)?;
    Ok(acp::ExtNotification::new(LODY_SESSION_USAGE_UPDATE_METHOD, Arc::from(params)))
}

/// The model a provider actually served.
///
/// A router can answer a request with a different model than the one it was
/// asked for, so the response wins whenever it names one. Resolving this once
/// keeps the reported attribution and the pricing profile describing the same
/// model.
fn effective_model<'a>(session_model: &'a str, response: &'a vtcode_core::llm::provider::LLMResponse) -> &'a str {
    if response.model.is_empty() {
        session_model
    } else {
        response.model.as_str()
    }
}

fn response_usage_notification(
    session_id: &acp::SessionId,
    model: &str,
    response: &vtcode_core::llm::provider::LLMResponse,
    profile: UsageProfile,
) -> anyhow::Result<Option<acp::ExtNotification>> {
    let Some(usage) = response.usage.as_ref() else {
        return Ok(None);
    };
    usage_notification(session_id, model, usage, profile).map(Some)
}

#[cfg(test)]
fn send_usage_update(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    model: &str,
    usage: &Usage,
    profile: UsageProfile,
) -> anyhow::Result<()> {
    client
        .send_ext_notification(usage_notification(session_id, model, usage, profile)?)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

/// Resolve the usage profile of the backend that served `provider`.
///
/// A configured custom provider wins over the built-in catalogue: its
/// resolved profile carries the context window and pricing the operator
/// declared, and its `api_format` names the backend that produced the usage.
/// Otherwise the built-in model catalogue supplies what it knows, and the
/// provider name alone decides the input-token semantics.
fn usage_profile(custom_providers: &[CustomProviderConfig], provider: &str, model: &str) -> UsageProfile {
    if let Some(custom_provider) = custom_providers
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(provider))
    {
        let resolved = custom_provider.resolved_profile(model);
        return UsageProfile {
            context_window: resolved.context_window.filter(|window| *window > 0),
            pricing: resolved.pricing.model_pricing(),
            exclusive_input: exclusive_input_semantics(resolved.api_format, provider),
        };
    }

    ModelResolver::resolve(Some(provider), model, &[], None).map_or_else(
        || UsageProfile {
            exclusive_input: exclusive_input_semantics(None, provider),
            ..UsageProfile::default()
        },
        |resolved| UsageProfile {
            context_window: resolved.context_window(),
            pricing: resolved.pricing(),
            exclusive_input: exclusive_input_semantics(None, provider),
        },
    )
}

impl ZedAgent {
    /// Look up the pricing and context window configured for this provider.
    ///
    /// Custom provider entries supply both; a provider without one reports
    /// neither, so no cost is invented for it.
    fn lody_usage_profile(&self, provider: &str, model: &str) -> UsageProfile {
        usage_profile(&self.workspace_runtime_config.custom_providers, provider, model)
    }

    pub(super) fn publish_lody_usage(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        fallback_model: &str,
        response: &vtcode_core::llm::provider::LLMResponse,
    ) {
        let Some(client) = self.client() else {
            return;
        };
        let model = effective_model(fallback_model, response);
        let profile = self.lody_usage_profile(provider, model);
        let notification = match response_usage_notification(session_id, model, response, profile) {
            Ok(Some(notification)) => notification,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, %session_id, "Failed to serialize Lody ACP usage update");
                return;
            }
        };
        if let Err(error) = client.send_ext_notification(notification) {
            tracing::warn!(%error, %session_id, "Failed to publish Lody ACP usage update");
        }
    }
}

#[cfg(test)]
#[path = "lody_usage/tests.rs"]
mod tests;
