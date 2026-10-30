//! Metric-contract scenarios for cached automatic compaction.

use super::*;

/// Adversarial provider, model, session, prompt, and error payloads stay private.
fn assert_metric_privacy(events: &[TelemetryEvent]) {
    let rendered = format!("{events:?}");
    for payload in [
        "MODEL_PAYLOAD",
        "SESSION_PAYLOAD",
        "PROMPT_PAYLOAD",
        "ERROR_PAYLOAD",
        "PROVIDER_PAYLOAD",
    ] {
        assert!(!rendered.contains(payload), "private runtime payload entered telemetry: {payload}");
    }
}

#[tokio::test]
async fn invalid_prefire_cache_native_failure_records_selected_provider_mode() {
    fn require_send<T: Send>(future: T) -> T {
        future
    }

    let mut scenario = AutoScenario::new(8_192);
    scenario.provider.native_compaction = true;
    let mut invalid_cache = scenario.cache("outdated prefix note".to_string());
    invalid_cache.fingerprint ^= 1;
    scenario.prefire.store(invalid_cache);
    let original = scenario.history.clone();

    assert!(require_send(scenario.run(true)).await.is_err());

    assert_eq!(scenario.history, original);
    assert!(!scenario.prefire.has_cache());
    assert!(
        scenario
            .provider
            .requests
            .lock()
            .expect("no cached or local generation")
            .is_empty()
    );
    assert_eq!(scenario.provider.native_requests.load(Ordering::Relaxed), 1);
    let metrics = scenario.recorded_metrics();
    assert_terminal_metrics(&metrics, "failure", "provider");
    assert!(
        metrics
            .iter()
            .skip(1)
            .all(|event| event.tags.get("mode").map(String::as_str) == Some("provider"))
    );
}

#[tokio::test]
async fn eligible_small_history_emits_no_change_without_provider_call() {
    let mut scenario = AutoScenario::new(8_192);
    scenario.engine.keep_last_messages = 10;
    let original = scenario.history.clone();

    assert!(
        scenario
            .run(false)
            .await
            .expect("eligible small history is unchanged")
            .is_none()
    );

    assert_eq!(scenario.history, original);
    assert!(scenario.provider.requests.lock().expect("requests").is_empty());
    let metrics = scenario.recorded_metrics();
    assert_eq!(metrics.len(), 3);
    assert_terminal_metrics(&metrics, "no_change", "none");
    assert!(
        metrics
            .iter()
            .any(|event| event.name == "vtcode.compaction.auto_no_change_total")
    );
}

#[tokio::test]
async fn provider_failure_metrics_exclude_all_runtime_payloads() {
    let mut scenario = AutoScenario::new(8_192);
    scenario.provider.failure = Some(format!("ERROR_PAYLOAD {MODEL} {SESSION} {PROMPT}"));
    scenario.prefire.store(scenario.cache("prefire note".to_string()));
    let original = scenario.history.clone();

    assert!(scenario.run(true).await.is_err());

    assert_eq!(scenario.history, original);
    let metrics = scenario.recorded_metrics();
    assert_eq!(metrics.len(), 3);
    assert_terminal_metrics(&metrics, "failure", "provider");
    assert_metric_privacy(&metrics);
    assert!(metrics.iter().all(|event| matches!(
        event.name.as_str(),
        "vtcode.compaction.auto_attempt_total"
            | "vtcode.compaction.auto_failure_total"
            | "vtcode.compaction.auto_duration_ms"
    )));
}

#[tokio::test]
async fn suppressed_and_ineligible_invocations_emit_no_attempt_metrics() {
    let mut scenario = AutoScenario::new(8_192);
    scenario.suppressed = crate::compaction::SUPPRESS_STICKY;
    assert!(scenario.run(true).await.expect("suppression skips").is_none());
    scenario.suppressed = SUPPRESS_NONE;
    scenario.config.agent.harness.auto_compaction_enabled = false;
    assert!(scenario.run(true).await.expect("disabled skips").is_none());
    scenario.config.agent.harness.auto_compaction_enabled = true;
    scenario.config.agent.harness.auto_compaction_threshold_tokens = Some(7_000);
    assert!(scenario.run(false).await.expect("below threshold skips").is_none());
    assert!(scenario.recorded_metrics().is_empty());
    assert!(scenario.provider.requests.lock().expect("requests").is_empty());
}

#[tokio::test]
async fn protected_budget_refusal_classification_uses_typed_causes_not_error_text() {
    let mut scenario = AutoScenario::new(2_048);
    scenario.history[0] = Message::user(PROMPT.repeat(1_000));
    let error = scenario.run(true).await.expect_err("protected anchors exceed context");
    let matching_text = error.to_string();
    assert!(crate::compaction::is_protected_user_budget_error(&error.context("nested caller context")));
    assert!(!crate::compaction::is_protected_user_budget_error(&anyhow::anyhow!(matching_text.clone())));
    let provider_error = anyhow::Error::new(LLMError::Provider { message: matching_text, metadata: None });
    assert!(!crate::compaction::is_protected_user_budget_error(&provider_error));
    let metrics = scenario.recorded_metrics();
    assert_terminal_metrics(&metrics, "failure", "protected_user_budget");
    assert_eq!(metrics.len(), 4);
    assert!(
        metrics
            .iter()
            .any(|event| event.name == "vtcode.compaction.auto_protected_budget_refusal_total")
    );
}
