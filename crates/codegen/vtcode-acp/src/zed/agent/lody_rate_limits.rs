//! Project observed provider headers into Lody's optional rate-limit contract.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use vtcode_commons::llm::RateLimitMetadata;

use super::{ZedAgent, lody::lody_capabilities_mut};
use crate::acp;

const UPDATE_METHOD: &str = "_lody/rate_limits/update";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitWindow {
    used_percent: f64,
    window_duration_seconds: u64,
    resets_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitScope<'a> {
    provider_id: &'a str,
    model_id: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RateLimit<'a> {
    limit_id: String,
    scope: RateLimitScope<'a>,
    limit_name: String,
    windows: Vec<RateLimitWindow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsSnapshot<'a> {
    rate_limits: Vec<RateLimit<'a>>,
    fetched_at_epoch_seconds: u64,
}

struct QuotaDimension {
    id: &'static str,
    label: &'static str,
    limit: Option<u64>,
    remaining: Option<u64>,
    window_seconds: u64,
    reset_epoch_seconds: Option<u64>,
}

pub(super) fn add_lody_rate_limit_capability(capabilities: &mut acp::AgentCapabilities) {
    if let Some(lody) = lody_capabilities_mut(capabilities) {
        // Push-only observations: no query API or account-wide quota is claimed.
        let _ = lody.insert("rateLimits".to_string(), serde_json::json!({ "version": 1 }));
    }
}

fn rate_limit_snapshot<'a>(
    provider: &'a str,
    model: &'a str,
    limits: &RateLimitMetadata,
    now: u64,
) -> Option<RateLimitsSnapshot<'a>> {
    let reset = limits
        .reset_after_millis
        .and_then(|millis| now.checked_add(millis.div_ceil(1_000)));
    let dimensions = [
        QuotaDimension {
            id: "requests-minute",
            label: "Requests/min",
            limit: limits.requests_limit_per_minute,
            remaining: limits.requests_remaining_per_minute,
            window_seconds: 60,
            reset_epoch_seconds: None,
        },
        QuotaDimension {
            id: "tokens-minute",
            label: "Tokens/min",
            limit: limits.tokens_limit_per_minute,
            remaining: limits.tokens_remaining_per_minute,
            window_seconds: 60,
            reset_epoch_seconds: None,
        },
        QuotaDimension {
            id: "requests-second",
            label: "Requests/s",
            limit: limits.requests_limit_per_second,
            remaining: limits.requests_remaining_per_second,
            window_seconds: 1,
            reset_epoch_seconds: reset,
        },
        QuotaDimension {
            id: "tokens-second",
            label: "Tokens/s",
            limit: limits.tokens_limit_per_second,
            remaining: limits.tokens_remaining_per_second,
            window_seconds: 1,
            reset_epoch_seconds: None,
        },
        QuotaDimension {
            id: "prompt-second",
            label: "Prompt tokens/s",
            limit: limits.prompt_tokens_limit_per_second,
            remaining: None,
            window_seconds: 1,
            reset_epoch_seconds: None,
        },
        QuotaDimension {
            id: "cache-adjusted-prompt-second",
            label: "Cache-adjusted prompt tokens/s",
            limit: limits.cache_adjusted_prompt_tokens_limit_per_second,
            remaining: None,
            window_seconds: 1,
            reset_epoch_seconds: None,
        },
        QuotaDimension {
            id: "generated-second",
            label: "Generated tokens/s",
            limit: limits.generated_tokens_limit_per_second,
            remaining: None,
            window_seconds: 1,
            reset_epoch_seconds: None,
        },
    ];
    let rate_limits = dimensions
        .into_iter()
        .filter_map(|dimension| {
            let limit = dimension.limit.filter(|limit| *limit > 0)?;
            let windows = dimension
                .remaining
                .filter(|remaining| *remaining <= limit)
                .map(|remaining| RateLimitWindow {
                    used_percent: percentage_used(limit, remaining),
                    window_duration_seconds: dimension.window_seconds,
                    resets_at_epoch_seconds: dimension.reset_epoch_seconds,
                })
                .into_iter()
                .collect();
            Some(RateLimit {
                limit_id: format!("{provider}:{model}:{}", dimension.id),
                scope: RateLimitScope { provider_id: provider, model_id: model },
                // Lody has no absolute quota field; retain it in the supported name.
                limit_name: format!("{} (limit {limit})", dimension.label),
                windows,
            })
        })
        .collect::<Vec<_>>();
    (!rate_limits.is_empty()).then_some(RateLimitsSnapshot { rate_limits, fetched_at_epoch_seconds: now })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Percentages are approximate display values; absolute u64 counts remain in notices."
)]
fn percentage_used(limit: u64, remaining: u64) -> f64 {
    ((limit.saturating_sub(remaining) as f64 / limit as f64) * 100.0).clamp(0.0, 100.0)
}

impl ZedAgent {
    pub(super) fn publish_lody_rate_limits(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        limits: &RateLimitMetadata,
    ) {
        let Some(client) = self.client() else {
            return;
        };
        let Some(session) = self.session_handle(session_id) else {
            return;
        };
        let model = match session.data.lock() {
            Ok(data) => data.model.clone(),
            Err(_) => return,
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let Some(snapshot) = rate_limit_snapshot(provider, &model, limits, now) else {
            return;
        };
        let params = match serde_json::value::to_raw_value(&snapshot) {
            Ok(params) => params,
            Err(error) => {
                tracing::warn!(%error, "Failed to serialize Lody rate-limit observations");
                return;
            }
        };
        if let Err(error) = client.send_ext_notification(acp::ExtNotification::new(UPDATE_METHOD, Arc::from(params))) {
            tracing::warn!(%error, %session_id, "Failed to publish Lody rate-limit observations");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn baseten_windows_have_utilization_without_invented_reset_times() {
        let limits = RateLimitMetadata {
            requests_limit_per_minute: Some(60),
            requests_remaining_per_minute: Some(15),
            ..Default::default()
        };
        let snapshot = serde_json::to_value(rate_limit_snapshot("baseten", "model", &limits, 100)).unwrap();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/fixtures/acp/lody_rate_limit_snapshot.json"
        )))
        .expect("Lody consumer fixture");
        assert_eq!(snapshot, fixture);
        let limit = &snapshot["rateLimits"][0];
        assert_eq!(limit["scope"]["providerId"], "baseten");
        assert_eq!(limit["scope"]["modelId"], "model");
        assert_eq!(limit["windows"][0]["usedPercent"], 75.0);
        assert_eq!(limit["windows"][0]["windowDurationSeconds"], 60);
        assert!(limit["windows"][0]["resetsAtEpochSeconds"].is_null());
    }

    #[test]
    fn fireworks_limit_only_headers_do_not_invent_utilization() {
        let limits = RateLimitMetadata {
            prompt_tokens_limit_per_second: Some(500),
            prompt_tokens: Some(100),
            ..Default::default()
        };
        let snapshot = serde_json::to_value(rate_limit_snapshot("fireworks", "model", &limits, 100)).unwrap();
        assert_eq!(snapshot["rateLimits"][0]["limitName"], "Prompt tokens/s (limit 500)");
        assert_eq!(snapshot["rateLimits"][0]["windows"], serde_json::json!([]));
    }

    #[test]
    fn reset_interval_applies_only_to_together_request_window() {
        let limits = RateLimitMetadata {
            requests_limit_per_second: Some(10),
            requests_remaining_per_second: Some(0),
            tokens_limit_per_second: Some(100),
            tokens_remaining_per_second: Some(10),
            reset_after_millis: Some(1_250),
            ..Default::default()
        };
        let snapshot = rate_limit_snapshot("together", "model", &limits, 100).unwrap();
        assert_eq!(snapshot.rate_limits[0].windows[0].resets_at_epoch_seconds, Some(102));
        assert_eq!(snapshot.rate_limits[1].windows[0].resets_at_epoch_seconds, None);
    }

    #[test]
    fn missing_zero_and_inconsistent_counts_cannot_fabricate_quota() {
        assert!(rate_limit_snapshot("provider", "model", &RateLimitMetadata::default(), 0).is_none());
        let mut limits = RateLimitMetadata {
            requests_limit_per_minute: Some(0),
            requests_remaining_per_minute: Some(0),
            ..Default::default()
        };
        assert!(rate_limit_snapshot("provider", "model", &limits, 0).is_none());
        limits.requests_limit_per_minute = Some(5);
        limits.requests_remaining_per_minute = Some(6);
        assert!(
            rate_limit_snapshot("provider", "model", &limits, 0).unwrap().rate_limits[0]
                .windows
                .is_empty()
        );
    }

    proptest! {
        #[test]
        fn valid_counts_produce_bounded_finite_utilization(limit in 1u64..=u64::MAX, remaining in any::<u64>()) {
            let remaining = remaining.min(limit);
            let used = percentage_used(limit, remaining);
            prop_assert!(used.is_finite());
            prop_assert!((0.0..=100.0).contains(&used));
            prop_assert!(percentage_used(limit, limit).abs() < f64::EPSILON);
            prop_assert!((percentage_used(limit, 0) - 100.0).abs() < f64::EPSILON);
        }
    }
}
