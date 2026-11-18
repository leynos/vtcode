//! Tests for prompt-pressure accounting in the context manager.

use super::super::{ContextManager, SystemPromptParams};
use diapason_core::llm::provider as uni;
use hashbrown::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[test]
fn test_update_token_usage_prefers_prompt_pressure() {
    let mut manager = ContextManager::new("sys".into(), (), Arc::new(RwLock::new(HashMap::new())), None);

    // Initial state
    assert_eq!(manager.current_token_usage(), 0);

    // Update with first response: prompt-side pressure becomes authoritative.
    manager.update_token_usage(&Some(uni::Usage {
        prompt_tokens: 1000,
        completion_tokens: 500,
        reasoning_output_tokens: None,
        total_tokens: 1500,
        cached_prompt_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        iterations: None,
    }));
    assert_eq!(manager.current_token_usage(), 1000);

    // Update with second response: usage tracks latest prompt pressure, not cumulative output.
    manager.update_token_usage(&Some(uni::Usage {
        prompt_tokens: 2500,
        completion_tokens: 800,
        reasoning_output_tokens: None,
        total_tokens: 3300,
        cached_prompt_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        iterations: None,
    }));
    assert_eq!(manager.current_token_usage(), 2500);
}

#[test]
fn test_update_token_usage_falls_back_when_prompt_missing() {
    let mut manager = ContextManager::new("sys".into(), (), Arc::new(RwLock::new(HashMap::new())), None);

    manager.update_token_usage(&Some(uni::Usage {
        prompt_tokens: 0,
        completion_tokens: 800,
        reasoning_output_tokens: None,
        total_tokens: 3300,
        cached_prompt_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        iterations: None,
    }));

    // Fallback estimate = total - completion.
    assert_eq!(manager.current_token_usage(), 2500);
}

#[test]
fn test_prompt_estimate_includes_composed_instruction_growth() {
    let mut manager = ContextManager::new("sys".into(), (), Arc::new(RwLock::new(HashMap::new())), None);

    manager.record_prompt_estimate(160_000);
    assert_eq!(manager.current_token_usage(), 160_000);

    // A smaller estimate must not erase pressure observed from a larger
    // assembled prompt before the provider returns authoritative usage.
    manager.record_prompt_estimate(12_000);
    assert_eq!(manager.current_token_usage(), 160_000);
}

#[test]
fn test_compaction_resets_prompt_pressure_for_the_next_request() {
    let mut manager = ContextManager::new("sys".into(), (), Arc::new(RwLock::new(HashMap::new())), None);

    manager.record_prompt_estimate(160_000);
    assert!(manager.mark_compaction_pending_at_soft_threshold(Some(160_000)));

    manager.reset_token_pressure_after_compaction();

    assert_eq!(manager.current_token_usage(), 0);
    assert!(!manager.compaction_pending());
}

#[tokio::test]
async fn build_system_prompt_ignores_token_usage_updates() {
    let mut manager = ContextManager::new("System prompt".to_string(), (), Arc::new(RwLock::new(HashMap::new())), None);

    let params = SystemPromptParams {
        full_auto: false,
        planning_active: false,
        request_user_input_enabled: true,
    };

    let prompt_before = manager
        .build_system_prompt(params.clone())
        .await
        .expect("prompt before token update");

    manager.update_token_usage(&Some(uni::Usage {
        prompt_tokens: 180_000,
        completion_tokens: 2_000,
        reasoning_output_tokens: None,
        total_tokens: 182_000,
        cached_prompt_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        iterations: None,
    }));

    let prompt_after = manager.build_system_prompt(params).await.expect("prompt after token update");

    assert_eq!(prompt_before, prompt_after);
    assert!(!prompt_after.contains("<budget:token_budget>"));
    assert!(!prompt_after.contains("token_usage:"));
    assert!(!prompt_after.contains("[Context]"));
    assert!(!prompt_after.contains("[Runtime Context]"));
}
