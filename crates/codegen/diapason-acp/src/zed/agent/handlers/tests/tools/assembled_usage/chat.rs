//! Assembled usage for the OpenAI Chat wire format.

use super::*;

#[tokio::test]
async fn terminal_only_baseten_usage_reaches_the_acp_client() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, TERMINAL_USAGE_STREAM, "/chat/completions").await;
    let workspace = TempDir::new().expect("Baseten usage workspace");
    let notifications =
        run_assembled_prompt(&baseten_core_config(workspace.path()), &baseten_vt_config(&server.uri())).await;

    let updates = usage_updates(&notifications);
    assert_eq!(
        updates.len(),
        1,
        "one response with terminal-only usage must publish exactly one usage delta: {updates:?}"
    );
    let update = &updates[0];
    assert_eq!(update["usage"]["inputTokens"], 13);
    assert_eq!(
        update["usage"]["outputTokens"], 2,
        "reasoning tokens are reported separately and excluded from the output count"
    );
    assert_eq!(update["usage"]["reasoningOutputTokens"], 3);
    assert_eq!(update["usage"]["contextWindow"], 131_072);
    // OpenAI Chat reports prompt tokens inclusive of cache tokens, and the
    // fixture declares no cache activity, so cost is the declared rates over
    // the parser's prompt and completion counts.
    let expected_cost = 13.0 * BASETEN_INPUT_PER_MILLION / 1_000_000.0 + 5.0 * BASETEN_OUTPUT_PER_MILLION / 1_000_000.0;
    let cost = update["usage"]["costUSD"]
        .as_f64()
        .expect("the declared rates must price the turn");
    assert!((cost - expected_cost).abs() < 1e-12, "expected {expected_cost} from the declared rates, got {cost}");
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["inputTokens"], 13);
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["outputTokens"], 2);
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["reasoningOutputTokens"], 3);

    // The assembled path, not just the projection: the real OpenAI-chat
    // adapter opted into streamed usage for this profile.
    let requests = requests.lock().expect("stream request mutex");
    assert!(!requests.is_empty(), "the prompt must reach the mock origin");
    for request in requests.iter() {
        assert_eq!(request["model"], BASETEN_MODEL);
        assert_eq!(
            request["stream_options"]["include_usage"], true,
            "an opted-in Baseten profile must request streamed usage"
        );
    }
}

/// An inclusive-input backend that reports cache activity must publish the
/// provider's prompt total unchanged, alongside the cache count and a cost that
/// prices cached and uncached tokens at their own rates.
///
/// The OpenAI-chat parser only lifts `prompt_tokens_details.cached_tokens` when
/// cache metrics are enabled, so this is also the end-to-end check that the
/// default configuration surfaces them.
#[tokio::test]
async fn inclusive_openai_cache_usage_reaches_both_usage_fields() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, OPENAI_CACHE_USAGE_STREAM, "/chat/completions").await;
    let workspace = TempDir::new().expect("OpenAI cache usage workspace");
    let notifications =
        run_assembled_prompt(&baseten_core_config(workspace.path()), &cache_priced_baseten_vt_config(&server.uri()))
            .await;

    let updates = usage_updates(&notifications);
    assert_eq!(updates.len(), 1, "one response with cached usage must publish exactly one usage delta: {updates:?}");
    let update = &updates[0];

    // 24 uncached + 5 cached. The OpenAI wire format already includes the
    // cached tokens in `prompt_tokens`, so the published total must be the raw
    // 29 rather than 29 plus the cache count.
    let expected_input = 29;
    for usage in [&update["usage"], &update["modelUsage"][BASETEN_MODEL]] {
        assert_eq!(usage["inputTokens"], expected_input);
        assert_eq!(usage["cacheReadInputTokens"], 5);
        assert_eq!(usage["outputTokens"], 8);
    }
    assert_eq!(update["usage"]["contextWindow"], 131_072);
    assert_eq!(
        update["modelUsage"][BASETEN_MODEL], update["usage"],
        "the per-model and session projections must agree for a single model"
    );

    let expected_cost = 24.0 * BASETEN_INPUT_PER_MILLION / 1_000_000.0
        + 5.0 * BASETEN_CACHE_READ_PER_MILLION / 1_000_000.0
        + 8.0 * BASETEN_OUTPUT_PER_MILLION / 1_000_000.0;
    let cost = update["usage"]["costUSD"]
        .as_f64()
        .expect("the declared rates must price the cached turn");
    assert!(
        (cost - expected_cost).abs() < 1e-12,
        "cached prompt tokens must be priced at the cache-read rate: expected {expected_cost}, got {cost}"
    );

    let requests = requests.lock().expect("stream request mutex");
    assert!(!requests.is_empty(), "the prompt must reach the mock origin");
}

/// Disabling OpenAI cache metrics must suppress the cache count without
/// changing the inclusive prompt total.
///
/// The wire stream still carries `prompt_tokens_details`, so a payload that
/// merely echoed the provider's JSON would still report five cached tokens.
/// The published count must instead be zero, and the turn is then priced as if
/// every prompt token were uncached.
#[tokio::test]
async fn disabled_cache_metrics_suppress_the_cache_count() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, OPENAI_CACHE_USAGE_STREAM, "/chat/completions").await;
    let workspace = TempDir::new().expect("OpenAI cache usage workspace");
    let notifications = run_assembled_prompt(
        &baseten_core_config_without_cache_metrics(workspace.path()),
        &cache_priced_baseten_vt_config(&server.uri()),
    )
    .await;

    let updates = usage_updates(&notifications);
    assert_eq!(updates.len(), 1, "one response with usage must publish exactly one usage delta: {updates:?}");
    let update = &updates[0];

    for usage in [&update["usage"], &update["modelUsage"][BASETEN_MODEL]] {
        assert_eq!(usage["inputTokens"], 29, "turning off cache metrics must not change the inclusive prompt total");
        assert_eq!(usage["cacheReadInputTokens"], 0, "disabled cache metrics must not surface the cache detail");
        assert_eq!(usage["outputTokens"], 8);
    }

    let expected_cost = 29.0 * BASETEN_INPUT_PER_MILLION / 1_000_000.0 + 8.0 * BASETEN_OUTPUT_PER_MILLION / 1_000_000.0;
    let cost = update["usage"]["costUSD"]
        .as_f64()
        .expect("the declared rates must price the turn");
    assert!(
        (cost - expected_cost).abs() < 1e-12,
        "without a cache count every prompt token is priced uncached: expected {expected_cost}, got {cost}"
    );

    let requests = requests.lock().expect("stream request mutex");
    assert!(!requests.is_empty(), "the prompt must reach the mock origin");
}

#[tokio::test]
async fn baseten_stream_without_usage_publishes_no_delta() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, USAGE_FREE_STREAM, "/chat/completions").await;
    let workspace = TempDir::new().expect("Baseten usage workspace");
    let notifications =
        run_assembled_prompt(&baseten_core_config(workspace.path()), &baseten_vt_config(&server.uri())).await;

    assert!(
        usage_updates(&notifications).is_empty(),
        "a provider stream that reports no usage must not invent a delta"
    );
    assert!(
        !requests.lock().expect("stream request mutex").is_empty(),
        "the prompt must still reach the mock origin"
    );
}
