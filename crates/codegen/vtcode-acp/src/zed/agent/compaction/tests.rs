//! Unit tests for ACP compaction budgeting and lifecycle modes.
use super::*;
use async_trait::async_trait;
use vtcode_core::llm::provider::{LLMError, LLMRequest, LLMResponse};

struct ContextProvider {
    context_size: usize,
}

#[async_trait]
impl LLMProvider for ContextProvider {
    fn name(&self) -> &str {
        "context-test"
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        self.context_size
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("pure admission tests do not generate")
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }
}

#[test]
fn arli_boundary_compacts_before_output_reservation_overflows_context() {
    let provider = ContextProvider { context_size: 524_288 };

    assert_eq!(admission_prompt_budget(&provider, "test"), Some(470_836));
    assert!(should_compact(&provider, "test", 507_905));
}

#[test]
fn prompt_below_soft_context_budget_does_not_compact() {
    let provider = ContextProvider { context_size: 524_288 };

    // The default 160,000-token session budget reaches its 90% threshold
    // before this provider's larger context window does.
    assert!(!should_compact(&provider, "test", 143_999));
}

#[test]
fn small_context_reserves_output_and_margin_without_exhausting_the_prompt_budget() {
    let provider = ContextProvider { context_size: 32_768 };

    assert_eq!(admission_prompt_budget(&provider, "test"), Some(15_360));
}

#[test]
fn admission_budget_scales_reservations_for_small_context_windows() {
    let provider = ContextProvider { context_size: 8_192 };

    assert_eq!(admission_prompt_budget(&provider, "test"), Some(3_277));
    assert!(!should_compact(&provider, "test", 3_276));
    assert!(should_compact(&provider, "test", 3_277));

    let smaller_provider = ContextProvider { context_size: 4_096 };
    assert_eq!(admission_prompt_budget(&smaller_provider, "test"), Some(1_639));

    let unknown_context_provider = ContextProvider { context_size: 0 };
    assert_eq!(admission_prompt_budget(&unknown_context_provider, "test"), None);
}

#[test]
fn lifecycle_compaction_mode_preserves_local_and_native_strategy() {
    assert_eq!(lifecycle_compaction_mode(CompactionStrategy::Local), CompactionMode::Local);
    assert_eq!(lifecycle_compaction_mode(CompactionStrategy::NativeStandalone), CompactionMode::Provider);
    assert_eq!(lifecycle_compaction_mode(CompactionStrategy::NativeInline), CompactionMode::Provider);
}
