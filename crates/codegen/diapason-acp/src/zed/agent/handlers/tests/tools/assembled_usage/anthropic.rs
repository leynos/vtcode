//! Assembled usage for the Anthropic Messages wire format.

use super::*;

/// An exclusive-input backend that also reports cache activity must publish a
/// prompt total that includes the cached tokens, alongside both cache counts.
///
/// This is the end-to-end counterpart to the unit tests on
/// `normalized_turn_usage`: the OpenAI-chat fixture proves inclusive semantics,
/// this one proves exclusive semantics are added back. Asserting on both
/// `usage` and `modelUsage` keeps the two projections from drifting apart.
#[tokio::test]
async fn exclusive_anthropic_cache_usage_reaches_both_usage_fields() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, ANTHROPIC_CACHE_USAGE_STREAM, "/v1/messages").await;
    let workspace = TempDir::new().expect("Anthropic usage workspace");
    let base_url = format!("{}/v1", server.uri());
    let notifications =
        run_assembled_prompt(&anthropic_core_config(workspace.path()), &anthropic_vt_config(&base_url)).await;

    let updates = usage_updates(&notifications);
    assert_eq!(updates.len(), 1, "one response with split usage must publish exactly one usage delta: {updates:?}");
    let update = &updates[0];

    // 21 uncached + 5 cache-read + 3 cache-creation. The Anthropic wire format
    // reports `input_tokens` exclusive of the cache counts, so publishing the
    // raw value would understate the prompt by the cached tokens.
    let expected_input = 21 + 5 + 3;
    for usage in [&update["usage"], &update["modelUsage"][ANTHROPIC_MODEL]] {
        assert_eq!(usage["inputTokens"], expected_input);
        assert_eq!(usage["cacheReadInputTokens"], 5);
        assert_eq!(usage["cacheCreationInputTokens"], 3);
        assert_eq!(usage["outputTokens"], 8);
    }
    assert_eq!(update["usage"]["contextWindow"], 131_072);
    assert_eq!(
        update["modelUsage"][ANTHROPIC_MODEL], update["usage"],
        "the per-model and session projections must agree for a single model"
    );

    let requests = requests.lock().expect("stream request mutex");
    assert!(!requests.is_empty(), "the prompt must reach the mock origin");
}
