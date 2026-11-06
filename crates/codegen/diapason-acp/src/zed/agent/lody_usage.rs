//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.

use crate::acp;
#[cfg(test)]
use crate::zed::connection::ConnectionHandle;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{field, info_span, warn};
use diapason_config::models::ModelPricing;
use diapason_core::llm::Usage;
use diapason_core::llm::model_resolver::ModelResolver;
use diapason_core::llm::usage_cost::{estimate_session_costs_with_pricing, normalized_turn_usage};

use super::redaction::detail_correlation;
use super::task_lifecycle::bounded_session_correlation;
use super::{ZedAgent, lody::lody_capabilities_mut};

use diapason_config::CustomProviderApiFormat;
use diapason_config::models::ModelPricing;
use diapason_core::llm::Usage;
use diapason_core::llm::model_resolver::ModelResolver;
use diapason_core::llm::usage_cost::{
    estimate_session_costs_with_pricing, normalized_turn_usage_with_input_semantics, provider_reports_exclusive_input,
};

//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
#[cfg(test)]
};

use diapason_config::models::ModelPricing;
use diapason_config::{CustomProviderApiFormat, CustomProviderConfig};
use diapason_core::llm::Usage;
use diapason_core::llm::model_resolver::ModelResolver;
use diapason_core::llm::usage_cost::{
    estimate_session_costs_with_pricing, normalized_turn_usage_with_input_semantics, provider_reports_exclusive_input,
};

//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
#[cfg(test)]
};
//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
#[cfg(test)]
};

use vtcode_core::llm::Usage;
use vtcode_core::llm::usage_cost::{TurnUsage, UsageProfile, resolve_turn_usage, usage_profile};

//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
<<<<<<< ours
#[cfg(test)]
};
//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
#[cfg(test)]
};
};
//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
#[cfg(test)]
};
//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.
#[cfg(test)]
};
||||||| original
#[cfg(test)]
};
=======
#[cfg(test)]
>>>>>>> theirs

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

/// Project one turn's resolved usage into the Lody delta contract.
///
/// The domain boundary has already decided input semantics, reasoning/output
/// split, and cost; this function only names those values on the wire. Every
/// optional field stays absent when the domain resolved none, so the payload
/// never claims a window or cost the provider did not supply.
fn usage_notification(
    session_id: &acp::SessionId,
    model: &str,
    usage: TurnUsage,
) -> anyhow::Result<acp::ExtNotification> {
    let delta = LodyUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        cost_usd: usage.cost_usd,
        context_window: usage.context_window,
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
fn effective_model<'a>(session_model: &'a str, response: &'a diapason_core::llm::provider::LLMResponse) -> &'a str {
    if response.model.is_empty() {
        session_model
    } else {
        response.model.as_str()
    }
}

fn response_usage_notification(
    session_id: &acp::SessionId,
    model: &str,
    response: &diapason_core::llm::provider::LLMResponse,
    profile: UsageProfile,
) -> anyhow::Result<Option<acp::ExtNotification>> {
    let Some(usage) = response.usage.as_ref() else {
        return Ok(None);
    };
    usage_notification(session_id, model, resolve_turn_usage(usage, profile)).map(Some)
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
        .send_ext_notification(usage_notification(session_id, model, resolve_turn_usage(usage, profile))?)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn usage_elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Records a failed usage publication on its span and in the bounded metrics.
///
/// The session is identified only by its bounded correlation hash. The
/// transport or serialization error may embed provider payload, paths, URLs or
/// personal data, so the warning carries fixed fields and a bounded digest
/// rather than the error's rendering.
fn record_usage_failure(
    span: &tracing::Span,
    started_at: Instant,
    metrics: &diapason_core::metrics::MetricsCollector,
    error_category: &'static str,
    session_correlation: &str,
    error: &dyn std::fmt::Display,
) {
    let elapsed_ms = usage_elapsed_ms(started_at);
    let detail = error.to_string();
    let _outcome_recorded = span.record("outcome", "failed");
    let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_ms);
    let _error_category_recorded = span.record("error_category", error_category);
    metrics.record_acp_usage_notification(false);
    warn!(
        session_correlation,
        error_category,
        elapsed_ms,
        error_bytes = detail.len(),
        detail_correlation = %detail_correlation(&detail),
        "Failed to publish Lody ACP usage update"
    );
}

impl ZedAgent {
    pub(super) fn publish_lody_usage(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        fallback_model: &str,
        response: &diapason_core::llm::provider::LLMResponse,
    ) {
        let Some(client) = self.client() else {
            return;
        };
        let metrics = self.local_tool_registry.metrics_collector();
        let session_correlation = bounded_session_correlation(session_id);
        let span = info_span!(
            "acp_lody_usage_notification",
            session_correlation = %session_correlation,
            outcome = field::Empty,
            elapsed_ms = field::Empty,
            error_category = field::Empty
        );
        let _span_entered = span.enter();
        let started_at = Instant::now();
        let model = effective_model(fallback_model, response);
        let profile = usage_profile(&self.workspace_runtime_config.custom_providers, provider, model);
        let notification = match response_usage_notification(session_id, model, response, profile) {
            Ok(Some(notification)) => notification,
            Ok(None) => {
                let _outcome_recorded = span.record("outcome", "no_usage");
                let _elapsed_ms_recorded = span.record("elapsed_ms", usage_elapsed_ms(started_at));
                return;
            }
            Err(error) => {
                record_usage_failure(&span, started_at, &metrics, "serialization_failed", &session_correlation, &error);
                return;
            }
        };
        if let Err(error) = client.send_ext_notification(notification) {
            record_usage_failure(&span, started_at, &metrics, "notification_send_failed", &session_correlation, &error);
            return;
        }
        let _outcome_recorded = span.record("outcome", "sent");
        let _elapsed_ms_recorded = span.record("elapsed_ms", usage_elapsed_ms(started_at));
        metrics.record_acp_usage_notification(true);
    }
}

#[cfg(test)]
#[path = "lody_usage/tests.rs"]
mod tests;
