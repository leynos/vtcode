//! Tests for usage cost estimation, turn resolution, and budget status.
//!
//! The budget decision is shared by the runner and the binary's turn loop, so
//! these cases pin the boundary semantics those two callers rely on.

use super::*;

use proptest::prelude::*;
use rstest::rstest;
use serde_json::json;

fn approx_eq(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-12, "expected {a} to approx-equal {b}");
}

#[test]
fn estimate_tool_definition_tokens_is_zero_for_empty_slice() {
    assert_eq!(estimate_tool_definition_tokens(&[]), 0);
}

#[test]
fn estimate_tool_definition_tokens_matches_serialized_byte_length() {
    let tool = ToolDefinition::function(
        "read_file".to_string(),
        "Read the contents of a file from the workspace.".to_string(),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"],
        }),
    );

    let expected_bytes = serde_json::to_string(&tool).expect("tool serializes").len() as u64;
    let expected_tokens = expected_bytes.div_ceil(4);

    assert_eq!(estimate_tool_definition_tokens(&[tool]), expected_tokens);
}

#[test]
fn explicit_input_semantics_select_the_cache_accounting() {
    let usage = ProviderUsage {
        prompt_tokens: 100,
        completion_tokens: 20,
        reasoning_output_tokens: None,
        total_tokens: 120,
        cached_prompt_tokens: None,
        cache_creation_tokens: Some(50),
        cache_read_tokens: Some(400),
        iterations: None,
    };

    let exclusive = normalized_turn_usage_with_input_semantics(true, &usage);
    assert_eq!(exclusive.input_tokens, 550);
    let inclusive = normalized_turn_usage_with_input_semantics(false, &usage);
    assert_eq!(inclusive.input_tokens, 100);
}

#[test]
fn cost_normalization_follows_the_selected_backend_not_the_provider_name() {
    // `baseten` is a custom name either way; only the backend decides
    // whether `prompt_tokens` already includes the cached tokens.
    let usage = ProviderUsage {
        prompt_tokens: 100,
        completion_tokens: 0,
        cached_prompt_tokens: Some(50),
        ..ProviderUsage::default()
    };
    let pricing = Some(ModelPricing {
        input: Some(1.0 / 1_000_000.0),
        output: Some(0.0),
        cache_read: Some(0.0),
        cache_write: Some(0.0),
    });
    let cost = |exclusive_input| {
        resolve_turn_usage(
            &usage,
            UsageProfile {
                pricing,
                exclusive_input,
                ..UsageProfile::default()
            },
        )
        .cost_usd
        .expect("costUSD")
    };

    assert!((cost(false) - 50.0 / 1_000_000.0).abs() < f64::EPSILON);
    assert!((cost(true) - 100.0 / 1_000_000.0).abs() < f64::EPSILON);
}

#[test]
fn resolved_turn_usage_reports_reasoning_separately_from_output() {
    let usage = ProviderUsage {
        prompt_tokens: 21,
        completion_tokens: 8,
        reasoning_output_tokens: Some(3),
        cached_prompt_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..ProviderUsage::default()
    };

    let resolved = resolve_turn_usage(&usage, UsageProfile::default());

    assert_eq!(resolved.input_tokens, 21);
    assert_eq!(resolved.output_tokens, 5);
    assert_eq!(resolved.reasoning_output_tokens, Some(3));
    assert_eq!(resolved.cache_read_input_tokens, 5);
    assert_eq!(resolved.cache_creation_input_tokens, Some(3));
    assert_eq!(resolved.cost_usd, None, "no pricing must not invent a cost");
    assert_eq!(resolved.context_window, None);
}

/// Reasoning tokens are hidden from the reported output count but remain
/// billable completion tokens, so the cost must charge for all eight tokens
/// even though only five are visible.
#[test]
fn resolved_turn_usage_prices_reasoning_tokens_hidden_from_the_output_count() {
    let usage = ProviderUsage {
        prompt_tokens: 0,
        completion_tokens: 8,
        reasoning_output_tokens: Some(3),
        ..ProviderUsage::default()
    };
    let profile = UsageProfile {
        pricing: Some(ModelPricing {
            input: Some(0.0),
            output: Some(1.0 / 1_000_000.0),
            cache_read: None,
            cache_write: None,
        }),
        ..UsageProfile::default()
    };

    let resolved = resolve_turn_usage(&usage, profile);

    assert_eq!(resolved.output_tokens, 5, "reasoning is reported separately from the visible output");
    assert_eq!(resolved.reasoning_output_tokens, Some(3));
    approx_eq(resolved.cost_usd.expect("priced turn should report its cost"), 8.0 / 1_000_000.0);
}

#[test]
fn resolved_turn_usage_clamps_reasoning_to_the_completion_count() {
    let usage = ProviderUsage {
        prompt_tokens: 4,
        completion_tokens: 2,
        reasoning_output_tokens: Some(9),
        ..ProviderUsage::default()
    };

    let resolved = resolve_turn_usage(&usage, UsageProfile::default());

    assert_eq!(resolved.reasoning_output_tokens, Some(2));
    assert_eq!(resolved.output_tokens, 0, "reasoning must never exceed the completion count");
}

#[test]
fn resolved_turn_usage_reports_the_total_prompt_tokens_for_exclusive_input() {
    let usage = ProviderUsage {
        prompt_tokens: 100,
        completion_tokens: 20,
        cache_read_tokens: Some(400),
        cache_creation_tokens: Some(50),
        ..ProviderUsage::default()
    };

    let resolved = resolve_turn_usage(&usage, UsageProfile { exclusive_input: true, ..UsageProfile::default() });

    assert_eq!(
        resolved.input_tokens, 550,
        "an exclusive prompt count must be reported as the total the contract documents"
    );
    assert_eq!(resolved.cache_read_input_tokens, 400);
    assert_eq!(resolved.cache_creation_input_tokens, Some(50));
}

#[test]
fn resolved_turn_usage_keeps_inclusive_prompt_tokens_as_reported() {
    let usage = ProviderUsage {
        prompt_tokens: 500,
        completion_tokens: 20,
        cached_prompt_tokens: Some(400),
        ..ProviderUsage::default()
    };

    let resolved = resolve_turn_usage(&usage, UsageProfile::default());

    assert_eq!(resolved.input_tokens, 500, "an inclusive prompt count already contains the cached tokens");
    assert_eq!(resolved.cache_read_input_tokens, 400);
}

#[test]
fn resolved_turn_usage_saturates_an_input_total_that_overflows_u32() {
    let usage = ProviderUsage {
        prompt_tokens: u32::MAX,
        cache_read_tokens: Some(u32::MAX),
        ..ProviderUsage::default()
    };

    let resolved = resolve_turn_usage(&usage, UsageProfile { exclusive_input: true, ..UsageProfile::default() });

    assert_eq!(resolved.input_tokens, u32::MAX, "an overflowing total is reported at the ceiling");
}

proptest! {
    /// The reported total is the provider's own count, plus the cache
    /// counts only when the backend excludes them, saturating at `u32::MAX`.
    ///
    /// Both representations are generated, the cache counts are optional,
    /// and the boundary values `0` and `u32::MAX` are drawn deliberately
    /// rather than left to chance, so overflow and absence are exercised.
    #[test]
    fn resolved_input_tokens_follow_the_selected_semantics(
        prompt in token_count(),
        cache_read in proptest::option::of(token_count()),
        cached_prompt in proptest::option::of(token_count()),
        cache_creation in proptest::option::of(token_count()),
        exclusive_input in any::<bool>(),
    ) {
        let usage = ProviderUsage {
            prompt_tokens: prompt,
            cache_read_tokens: cache_read,
            cached_prompt_tokens: cached_prompt,
            cache_creation_tokens: cache_creation,
            ..ProviderUsage::default()
        };
        // The documented contract: a cache-read count falls back to the
        // provider's older cached-prompt field, and an absent count is zero.
        let cached = cache_read.or(cached_prompt).unwrap_or(0);
        let creation = cache_creation.unwrap_or(0);
        let total = if exclusive_input {
            u64::from(prompt) + u64::from(cached) + u64::from(creation)
        } else {
            u64::from(prompt)
        };
        let expected = u32::try_from(total).unwrap_or(u32::MAX);

        let resolved = resolve_turn_usage(
            &usage,
            UsageProfile { exclusive_input, ..UsageProfile::default() },
        );

        prop_assert_eq!(resolved.input_tokens, expected);
        prop_assert_eq!(resolved.cache_read_input_tokens, cached);
        prop_assert_eq!(resolved.cache_creation_input_tokens, cache_creation);
    }
}

/// A token count that reaches the boundaries of the released range.
fn token_count() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0u32), Just(u32::MAX), Just(u32::MAX - 1), 1u32..=u32::MAX - 2]
}

#[test]
fn estimate_session_costs_with_pricing_discounts_cache_reads() {
    let pricing = ModelPricing {
        input: Some(0.01),
        output: Some(0.02),
        cache_read: Some(0.001),
        cache_write: Some(0.0125),
    };
    let usage = diapason_exec_events::Usage {
        input_tokens: 1_000,
        cached_input_tokens: 800,
        cache_creation_tokens: 0,
        output_tokens: 100,
    };

    let estimate = estimate_session_costs_with_pricing(pricing, &usage).expect("estimate");

    // raw: all 1000 input tokens at full rate + output.
    approx_eq(estimate.raw_usd, 1_000.0 * 0.01 + 100.0 * 0.02);
    // effective: 200 uncached @ input rate + 800 cached @ read rate + output.
    approx_eq(estimate.effective_usd, 200.0 * 0.01 + 800.0 * 0.001 + 100.0 * 0.02);
    assert!(estimate.effective_usd < estimate.raw_usd);
}

#[test]
fn estimate_session_costs_with_pricing_matches_raw_when_no_cache_activity() {
    let pricing = ModelPricing {
        input: Some(0.01),
        output: Some(0.02),
        cache_read: Some(0.001),
        cache_write: Some(0.0125),
    };
    let usage = diapason_exec_events::Usage {
        input_tokens: 1_000,
        cached_input_tokens: 0,
        cache_creation_tokens: 0,
        output_tokens: 100,
    };

    let estimate = estimate_session_costs_with_pricing(pricing, &usage).expect("estimate");
    approx_eq(estimate.raw_usd, estimate.effective_usd);
}

#[test]
fn estimate_session_costs_with_pricing_uses_heuristic_fallback_rates() {
    let pricing = ModelPricing {
        input: Some(0.01),
        output: Some(0.02),
        cache_read: None,
        cache_write: None,
    };
    let usage = diapason_exec_events::Usage {
        input_tokens: 1_000,
        cached_input_tokens: 50,
        cache_creation_tokens: 500,
        output_tokens: 50,
    };

    let estimate = estimate_session_costs_with_pricing(pricing, &usage).expect("estimate");

    let read_rate = 0.01 * 0.10;
    let write_rate = 0.01 * 1.25;
    let uncached = 1_000.0 - 50.0 - 500.0;
    let expected_effective = uncached * 0.01 + 50.0 * read_rate + 500.0 * write_rate + 50.0 * 0.02;
    approx_eq(estimate.effective_usd, expected_effective);
    approx_eq(estimate.raw_usd, 1_000.0 * 0.01 + 50.0 * 0.02);
    // Cache-creation tokens dominate here (500 vs. 50 cache-read tokens),
    // so the write-rate premium outweighs the read-rate discount and
    // pushes effective above raw.
    assert!(estimate.effective_usd > estimate.raw_usd);
}

#[test]
fn estimate_session_costs_with_pricing_returns_none_without_full_pricing() {
    let missing_input = ModelPricing {
        input: None,
        output: Some(0.02),
        cache_read: None,
        cache_write: None,
    };
    let missing_output = ModelPricing {
        input: Some(0.01),
        output: None,
        cache_read: None,
        cache_write: None,
    };
    let usage = diapason_exec_events::Usage::default();

    assert!(estimate_session_costs_with_pricing(missing_input, &usage).is_none());
    assert!(estimate_session_costs_with_pricing(missing_output, &usage).is_none());
}

#[test]
fn session_budget_tracks_spend_and_thresholds() {
    let mut budget = SessionBudget::new(Some(1.0));
    assert_eq!(budget.status(), BudgetStatus::Ok);
    // 0.5 -> Ok
    assert_eq!(budget.record(0.5), BudgetStatus::Ok);
    // 0.3 -> 0.8 >= 0.75 cap -> Warning
    assert_eq!(budget.record(0.3), BudgetStatus::Warning { spent: 0.8, max: 1.0 });
    // 0.3 -> 1.1 >= cap -> Exceeded
    assert_eq!(budget.record(0.3), BudgetStatus::Exceeded { spent: 1.1, max: 1.0 });
    assert!((budget.spent_usd() - 1.1).abs() < 1e-9);
    assert!((budget.remaining_usd().unwrap() - 0.0).abs() < 1e-9);
}

#[test]
fn session_budget_unlimited_is_always_ok() {
    let mut budget = SessionBudget::new(None);
    assert_eq!(budget.record(1000.0), BudgetStatus::Ok);
    assert_eq!(budget.remaining_usd(), None);
}

/// The warning threshold is inclusive, and reaching the cap is still a
/// warning: only spending strictly above the cap is `Exceeded`.
#[rstest]
#[case(0.75)]
#[case(1.0)]
fn budget_status_classify_reports_the_full_warning_status_at_the_boundaries(#[case] spent: f64) {
    assert_eq!(
        BudgetStatus::classify(spent, Some(1.0), 0.75),
        BudgetStatus::Warning { spent, max: 1.0 },
        "spend {spent} at the warning threshold or cap must classify as Warning"
    );
}

#[test]
fn budget_status_classify_matches_harness_semantics() {
    // Unlimited.
    assert_eq!(BudgetStatus::classify(999.0, None, 0.75), BudgetStatus::Ok);
    // Under warning.
    assert_eq!(BudgetStatus::classify(0.5, Some(1.0), 0.75), BudgetStatus::Ok);
    // Over cap.
    assert!(BudgetStatus::classify(1.01, Some(1.0), 0.75).is_exceeded());
    // Configurable threshold.
    assert_eq!(BudgetStatus::classify(0.6, Some(1.0), 0.5), BudgetStatus::Warning { spent: 0.6, max: 1.0 });
}
