//! Tests for the Lody usage extension notification.
//!
//! The domain boundary owns profile resolution and cost normalization; these
//! tests keep the adapter honest about projecting the resolved turn onto the
//! Lody wire contract, including the optional fields it must not invent.

use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use diapason_config::models::ModelPricing;
use diapason_config::{
    CustomProviderApiFormat, CustomProviderConfig, CustomProviderPricingConfig, CustomProviderProfileConfig,
};
use proptest::prelude::*;
use tokio::sync::{Notify, mpsc};

#[path = "tests/transport.rs"]
mod transport;

#[path = "tests/properties.rs"]
mod properties;

/// Resolve `usage` for the Lody projection with the given profile.
fn resolved_notification(usage: &Usage, profile: UsageProfile) -> serde_json::Value {
    let notification =
        usage_notification(&acp::SessionId::new(Arc::from("session-1")), "model-a", resolve_turn_usage(usage, profile))
            .expect("usage notification");
    serde_json::from_str(notification.params.get()).expect("usage JSON")
}

/// Assert resolved usage maps to the Lody delta contract.
#[test]
fn maps_resolved_usage_to_the_lody_delta_contract() {
    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 8,
        reasoning_output_tokens: Some(3),
        cached_prompt_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };
    let value = resolved_notification(
        &usage,
        UsageProfile {
            context_window: Some(1_048_576),
            pricing: Some(ModelPricing {
                input: Some(0.15 / 1_000_000.0),
                output: Some(0.50 / 1_000_000.0),
                cache_read: Some(0.03 / 1_000_000.0),
                cache_write: None,
            }),
            exclusive_input: false,
        },
    );

    assert_eq!(value["sessionId"], "session-1");
    assert_eq!(value["usage"]["inputTokens"], 21);
    assert_eq!(value["usage"]["outputTokens"], 5);
    assert_eq!(value["usage"]["reasoningOutputTokens"], 3);
    assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);
    assert_eq!(value["usage"]["contextWindow"], 1_048_576);
    assert!(value["usage"]["costUSD"].as_f64().is_some_and(|cost| cost > 0.0));
    assert_eq!(value["modelUsage"]["model-a"], value["usage"]);
}

/// Assert an exclusive prompt count reaches the wire as the documented total.
///
/// Anthropic-style backends report `prompt_tokens` without the cache-read and
/// cache-creation tokens. The delta must publish the full prompt total in
/// `inputTokens` and keep the cached counts in their own fields, so a client
/// that adds the three fields together does not double-count.
#[test]
fn exclusive_prompt_count_reaches_the_delta_as_the_total() {
    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 8,
        cache_read_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };

    let value = resolved_notification(&usage, UsageProfile { exclusive_input: true, ..UsageProfile::default() });

    assert_eq!(value["usage"]["inputTokens"], 29, "21 uncached + 5 cached + 3 cache-creation");
    assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);
    assert_eq!(value["modelUsage"]["model-a"], value["usage"]);
}

/// Assert the inclusive and exclusive representations of one turn agree.
///
/// Both describe the same tokens; only the provider's own `prompt_tokens`
/// convention differs. The published `inputTokens` total must therefore match.
#[test]
fn inclusive_and_exclusive_representations_publish_the_same_input_total() {
    let cached = 5;
    let created = 3;
    let uncached = 21;
    let exclusive = Usage {
        prompt_tokens: uncached,
        cache_read_tokens: Some(cached),
        cache_creation_tokens: Some(created),
        ..Usage::default()
    };
    let inclusive = Usage {
        prompt_tokens: uncached + cached + created,
        cache_read_tokens: Some(cached),
        cache_creation_tokens: Some(created),
        ..Usage::default()
    };

    let exclusive_value =
        resolved_notification(&exclusive, UsageProfile { exclusive_input: true, ..UsageProfile::default() });
    let inclusive_value = resolved_notification(&inclusive, UsageProfile::default());

    assert_eq!(exclusive_value["usage"]["inputTokens"], 29);
    assert_eq!(inclusive_value["usage"]["inputTokens"], 29, "an inclusive count already contains the cached tokens");
    assert_eq!(inclusive_value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(exclusive_value["usage"]["cacheReadInputTokens"], 5);
}

/// Assert optional fields stay absent when the profile supplies none.
#[test]
fn absent_profile_fields_are_not_projected_as_zero_or_invented() {
    let usage = Usage {
        prompt_tokens: 6,
        completion_tokens: 2,
        ..Usage::default()
    };

    let value = resolved_notification(&usage, UsageProfile::default());

    assert!(value["usage"].get("costUSD").is_none(), "no declared rates must mean no cost");
    assert!(value["usage"].get("contextWindow").is_none(), "no declared window must stay absent");
    assert!(value["usage"].get("cacheCreationInputTokens").is_none());
    assert!(value["usage"].get("reasoningOutputTokens").is_none());
}

/// Build a custom provider with the fields usage reporting reads.
fn custom_provider(
    name: &str,
    api_format: CustomProviderApiFormat,
    context_window: Option<usize>,
    pricing: CustomProviderPricingConfig,
) -> CustomProviderConfig {
    CustomProviderConfig {
        name: name.to_string(),
        display_name: name.to_string(),
        base_url: "https://llm.example/v1".to_string(),
        api_format,
        context_window,
        pricing,
        ..CustomProviderConfig::default()
    }
}

/// Declare pricing in the per-million-token units configuration uses.
fn per_million(input: f64, output: f64) -> CustomProviderPricingConfig {
    CustomProviderPricingConfig {
        input_per_million_usd: Some(input),
        output_per_million_usd: Some(output),
        cache_read_per_million_usd: None,
        cache_write_per_million_usd: None,
    }
}

/// Assert the usage profile follows the model the provider actually served.
#[test]
fn response_pricing_follows_the_model_the_provider_served() {
    let providers = vec![CustomProviderConfig {
        context_window: Some(200_000),
        pricing: per_million(3.0, 15.0),
        profiles: BTreeMap::from([(
            "served-model".to_string(),
            CustomProviderProfileConfig {
                context_window: Some(32_768),
                pricing: per_million(0.25, 1.0),
                ..CustomProviderProfileConfig::default()
            },
        )]),
        ..custom_provider("acme", CustomProviderApiFormat::OpenAIChat, None, CustomProviderPricingConfig::default())
    }];
    let response = diapason_core::llm::provider::LLMResponse {
        model: "served-model".to_string(),
        ..diapason_core::llm::provider::LLMResponse::new("session-model", "answer")
    };

    let profile = usage_profile(&providers, "acme", effective_model("session-model", &response));

    assert_eq!(
        profile.context_window,
        Some(32_768),
        "pricing must describe the served model, not the session's requested model"
    );
}

/// Assert a custom provider profile drives the notification window and cost.
#[test]
fn custom_provider_profile_drives_the_notification_context_window_and_cost() {
    let providers = vec![custom_provider(
        "acme",
        CustomProviderApiFormat::AnthropicMessages,
        Some(200_000),
        per_million(3.0, 15.0),
    )];
    let profile = usage_profile(&providers, "acme", "acme-large");
    let usage = Usage {
        prompt_tokens: 1_000_000,
        completion_tokens: 1_000_000,
        ..Usage::default()
    };

    let value = resolved_notification(&usage, profile);

    assert_eq!(value["usage"]["contextWindow"], 200_000);
    let cost = value["usage"]["costUSD"].as_f64().expect("costUSD");
    assert!((cost - 18.0).abs() < 1e-9, "1M input at $3/M plus 1M output at $15/M, got {cost}");
}

/// Assert a response without usage produces no notification.
#[test]
fn response_without_usage_produces_no_notification() {
    let response = diapason_core::llm::provider::LLMResponse::new("model-a", "answer");

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        "fallback-model",
        &response,
        UsageProfile::default(),
    )
    .expect("optional usage notification");

    assert!(notification.is_none());
}

/// Read back the model keys a usage notification attributes the usage to.
fn reported_model_names(notification: acp::ExtNotification) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");
    let model_usage = value["modelUsage"].as_object().expect("modelUsage object");
    model_usage.keys().cloned().collect()
}

/// Assert usage is attributed to the model the provider reported.
#[test]
fn response_usage_is_attributed_to_the_model_the_provider_served() {
    let response = diapason_core::llm::provider::LLMResponse {
        model: "served-model".to_string(),
        usage: Some(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            ..Usage::default()
        }),
        ..diapason_core::llm::provider::LLMResponse::new("fallback-model", "answer")
    };

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        effective_model("fallback-model", &response),
        &response,
        UsageProfile::default(),
    )
    .expect("optional usage notification")
    .expect("usage notification");

    assert_eq!(reported_model_names(notification), vec!["served-model".to_string()]);
}

/// Captured WARN output from a tracing subscriber.
struct CapturedWarn(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedWarn {
    /// A new sink sharing the same buffer with any writer clone.
    fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(Vec::new())))
    }

    /// A writer handle that appends to this sink's buffer.
    fn writer(&self) -> Self {
        Self(Arc::clone(&self.0))
    }

    /// The captured output as text.
    fn text(&self) -> String {
        let bytes = self.0.lock().expect("captured log mutex").clone();
        String::from_utf8(bytes).expect("captured logs are UTF-8")
    }
}

impl std::io::Write for CapturedWarn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("captured log mutex").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedWarn {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.writer()
    }
}

/// A usage-publication failure logs fixed fields and bounded values only.
///
/// The send error can embed provider payload, archive paths, or credentials,
/// so the warning records its size and a digest rather than its rendering. The
/// session is client-visible too, so the record carries a bounded correlation
/// instead of the identifier the client supplied.
#[test]
fn usage_publication_failure_logs_a_bounded_diagnostic() {
    let logs = CapturedWarn::new();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(logs.writer())
        .finish();
    let metrics = diapason_core::metrics::MetricsCollector::new();
    let session_id = "session-9f3a-client-visible";
    let session_correlation = bounded_session_correlation(&acp::SessionId::new(Arc::from(session_id)));
    let detail = "transport rejected /home/user/private/session.json while forwarding sk-live-secret";

    tracing::subscriber::with_default(subscriber, || {
        let span = info_span!("acp_lody_usage_notification");
        let _entered = span.enter();
        record_usage_failure(
            &span,
            Instant::now(),
            &metrics,
            "notification_send_failed",
            &session_correlation,
            &detail,
        );
    });

    let captured = logs.text();
    assert!(
        captured.contains("Failed to publish Lody ACP usage update"),
        "the failure must be reported: {captured}"
    );
    assert!(
        captured.contains(&format!("session_correlation=\"{session_correlation}\"")),
        "the bounded session correlation must be recorded: {captured}"
    );
    assert!(
        captured.contains("error_category=\"notification_send_failed\""),
        "the closed-set category must be recorded: {captured}"
    );
    assert!(
        captured.contains(&format!("error_bytes={}", detail.len())),
        "the error size must be measured rather than rendered: {captured}"
    );
    assert!(
        captured.contains(&detail_correlation(detail)),
        "the bounded detail digest must be recorded: {captured}"
    );
    assert!(!captured.contains(session_id), "the raw session id must not be logged: {captured}");
    assert!(
        !captured.contains("/home/user/private/session.json"),
        "the error rendering must not be logged: {captured}"
    );
    assert!(!captured.contains("sk-live-secret"), "the error rendering must not be logged: {captured}");
}

/// Assert a response without a model uses the session model.
#[test]
fn response_without_a_model_falls_back_to_the_session_model() {
    let response = diapason_core::llm::provider::LLMResponse {
        model: String::new(),
        usage: Some(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            ..Usage::default()
        }),
        ..diapason_core::llm::provider::LLMResponse::new("fallback-model", "answer")
    };

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        effective_model("fallback-model", &response),
        &response,
        UsageProfile::default(),
    )
    .expect("optional usage notification")
    .expect("usage notification");

    assert_eq!(reported_model_names(notification), vec!["fallback-model".to_string()]);
}
