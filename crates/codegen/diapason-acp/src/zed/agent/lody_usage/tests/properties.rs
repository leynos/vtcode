//! Property tests for the Lody usage projection.
//!
//! The domain boundary resolves the input semantics and the cost. These
//! properties keep the adapter honest about the two things it must not change:
//! every resolved count survives serialization, and an inclusive and an
//! exclusive representation of one turn publish the same total and price.

use super::*;
use proptest::prelude::*;

/// Generate a `u32` count, favouring the values where a sum can saturate.
fn boundary_or_arbitrary_u32() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0u32), Just(1u32), Just(u32::MAX - 1), Just(u32::MAX), any::<u32>(),]
}

proptest! {
    /// Assert the resolved input total saturates rather than wrapping, and that
    /// both ACP input fields carry it.
    ///
    /// The optional cache counts mean both the "provider reported a cache
    /// count" and the "provider reported none" shapes are generated. The
    /// expected total is computed here from the generated counts rather than
    /// from the production resolver, so a regression that wraps, drops the
    /// cached tokens, or publishes the provider's raw count fails the property
    /// instead of agreeing with itself.
    #[test]
    fn resolved_input_total_saturates_and_reaches_both_usage_fields(
        uncached in boundary_or_arbitrary_u32(),
        cache_read in proptest::option::of(boundary_or_arbitrary_u32()),
        cache_creation in proptest::option::of(boundary_or_arbitrary_u32()),
    ) {
        let read = cache_read.unwrap_or(0);
        let creation = cache_creation.unwrap_or(0);
        // The documented total is uncached + cached + cache-creation tokens,
        // reported at the `u32` ceiling when the sum overflows.
        let expected_total = u32::try_from(
            u64::from(uncached) + u64::from(read) + u64::from(creation),
        )
        .unwrap_or(u32::MAX);
        let usage = Usage {
            prompt_tokens: uncached,
            completion_tokens: 0,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            ..Usage::default()
        };
        let exclusive = resolved_notification(&usage, UsageProfile { exclusive_input: true, ..UsageProfile::default() });
        // An inclusive representation of the same turn carries the total in
        // its own prompt count, saturating there exactly as the resolver does.
        let inclusive = resolved_notification(
            &Usage {
                prompt_tokens: uncached.saturating_add(read).saturating_add(creation),
                ..usage
            },
            UsageProfile::default(),
        );

        for (representation, value) in [("exclusive", &exclusive), ("inclusive", &inclusive)] {
            prop_assert_eq!(
                value["usage"]["inputTokens"].as_u64(),
                Some(u64::from(expected_total)),
                "{} usage.inputTokens must report the saturated total",
                representation
            );
            prop_assert_eq!(
                value["modelUsage"]["model-a"]["inputTokens"].as_u64(),
                Some(u64::from(expected_total)),
                "{} modelUsage.inputTokens must report the saturated total",
                representation
            );
            prop_assert_eq!(
                value["usage"]["cacheReadInputTokens"].as_u64(),
                Some(u64::from(read)),
                "{} usage.cacheReadInputTokens must report the cached count",
                representation
            );
            prop_assert_eq!(
                value["modelUsage"]["model-a"]["cacheReadInputTokens"].as_u64(),
                Some(u64::from(read)),
                "{} modelUsage.cacheReadInputTokens must report the cached count",
                representation
            );
            match cache_creation {
                Some(count) => {
                    prop_assert_eq!(
                        value["usage"]["cacheCreationInputTokens"].as_u64(),
                        Some(u64::from(count)),
                        "{} usage.cacheCreationInputTokens must report the written count",
                        representation
                    );
                    prop_assert_eq!(
                        value["modelUsage"]["model-a"]["cacheCreationInputTokens"].as_u64(),
                        Some(u64::from(count)),
                        "{} modelUsage.cacheCreationInputTokens must report the written count",
                        representation
                    );
                }
                None => {
                    prop_assert!(
                        value["usage"].get("cacheCreationInputTokens").is_none(),
                        "{} must not invent a cache-creation count",
                        representation
                    );
                    prop_assert!(
                        value["modelUsage"]["model-a"].get("cacheCreationInputTokens").is_none(),
                        "{} must not invent a cache-creation count",
                        representation
                    );
                }
            }
        }
        prop_assert_eq!(&exclusive["modelUsage"]["model-a"], &exclusive["usage"]);
        prop_assert_eq!(&inclusive["modelUsage"]["model-a"], &inclusive["usage"]);
    }

    /// Assert every resolved u32 usage count survives serialization.
    #[test]
    fn preserves_all_resolved_u32_usage_counts(
        input in any::<u32>(),
        output in any::<u32>(),
        cache_read in any::<u32>(),
        cache_creation in proptest::option::of(any::<u32>()),
        reasoning in proptest::option::of(any::<u32>()),
        model in "[^\\p{C}]{1,48}",
    ) {
        let resolved = TurnUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
            reasoning_output_tokens: reasoning,
            cost_usd: None,
            context_window: None,
        };
        let notification = usage_notification(
            &acp::SessionId::new(Arc::from("property-session")),
            &model,
            resolved,
        ).expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

        prop_assert_eq!(value["usage"]["inputTokens"].as_u64(), Some(u64::from(input)));
        prop_assert_eq!(value["usage"]["outputTokens"].as_u64(), Some(u64::from(output)));
        prop_assert_eq!(value["usage"]["cacheReadInputTokens"].as_u64(), Some(u64::from(cache_read)));
        match cache_creation {
            Some(count) => prop_assert_eq!(
                value["usage"]["cacheCreationInputTokens"].as_u64(),
                Some(u64::from(count)),
            ),
            None => prop_assert!(value["usage"].get("cacheCreationInputTokens").is_none()),
        }
        match reasoning {
            Some(count) => prop_assert_eq!(
                value["usage"]["reasoningOutputTokens"].as_u64(),
                Some(u64::from(count)),
            ),
            None => prop_assert!(value["usage"].get("reasoningOutputTokens").is_none()),
        }
        prop_assert_eq!(&value["modelUsage"][model.as_str()], &value["usage"]);
    }

    /// Assert equivalent inclusive and exclusive input representations price
    /// the same turn identically on the wire, and that both match an
    /// independently computed expected cost.
    ///
    /// `prompt_tokens` either already contains the cache-read and
    /// cache-creation tokens (OpenAI-style) or excludes them
    /// (Anthropic-style). The notification reports the normalized total, which
    /// adds the cache counts back for the exclusive form and leaves the
    /// inclusive form unchanged, and a cost that depends only on the
    /// underlying token set and the declared rates, including the optional
    /// cache rates.
    ///
    /// The expected value is derived here from the generated counts and rates
    /// rather than from the production estimator, so a regression that
    /// returns zero, ignores the rates, or double-counts reasoning tokens
    /// fails the property instead of merely agreeing with itself.
    #[test]
    fn equivalent_inclusive_and_exclusive_inputs_report_the_same_cost(
        uncached in 0u32..1_000_000,
        cache_read in 0u32..1_000_000,
        cache_creation in 0u32..1_000_000,
        output in 0u32..1_000_000,
        reasoning in proptest::option::of(0u32..1_000_000),
        input_rate in 0.0f64..100.0,
        output_rate in 0.0f64..100.0,
        cache_read_rate in proptest::option::of(0.0f64..100.0),
        cache_write_rate in proptest::option::of(0.0f64..100.0),
    ) {
        // Rates are declared per million tokens; the estimator works per token.
        let per_token = |rate: f64| rate / 1_000_000.0;
        let input_per_token = per_token(input_rate);
        let output_per_token = per_token(output_rate);
        let pricing = ModelPricing {
            input: Some(input_per_token),
            output: Some(output_per_token),
            cache_read: cache_read_rate.map(per_token),
            cache_write: cache_write_rate.map(per_token),
        };
        let profile = |exclusive_input| UsageProfile {
            pricing: Some(pricing),
            exclusive_input,
            ..UsageProfile::default()
        };
        let inclusive = Usage {
            prompt_tokens: uncached + cache_read + cache_creation,
            completion_tokens: output,
            reasoning_output_tokens: reasoning,
            cache_read_tokens: Some(cache_read),
            cache_creation_tokens: Some(cache_creation),
            ..Usage::default()
        };
        let exclusive = Usage {
            prompt_tokens: uncached,
            completion_tokens: output,
            reasoning_output_tokens: reasoning,
            cache_read_tokens: Some(cache_read),
            cache_creation_tokens: Some(cache_creation),
            ..Usage::default()
        };

        // Independent expectation: uncached input at the input rate, cache reads
        // at the declared rate or the 10% fallback, cache creations at the
        // declared rate or the 125% fallback, and the full completion count once
        // at the output rate. Reasoning tokens are a subset of the completion
// count and are not charged again.
        let read_rate = cache_read_rate.map(per_token).unwrap_or(input_per_token * 0.10);
        let write_rate = cache_write_rate.map(per_token).unwrap_or(input_per_token * 1.25);
        let expected_cost = f64::from(uncached) * input_per_token
            + f64::from(cache_read) * read_rate
            + f64::from(cache_creation) * write_rate
            + f64::from(output) * output_per_token;

        let inclusive_value = resolved_notification(&inclusive, profile(false));
        let exclusive_value = resolved_notification(&exclusive, profile(true));
        let inclusive_cost = inclusive_value["usage"]["costUSD"].as_f64().expect("inclusive costUSD");
        let exclusive_cost = exclusive_value["usage"]["costUSD"].as_f64().expect("exclusive costUSD");

        // Both representations describe the same token set, so the ACP
        // `inputTokens` total is the full prompt count in both cases and the
        // cached count is reported beside it rather than double-counted.
        let full_prompt = uncached + cache_read + cache_creation;
        for (representation, value) in [("inclusive", &inclusive_value), ("exclusive", &exclusive_value)] {
            prop_assert_eq!(
                value["usage"]["inputTokens"].as_u64(),
                Some(u64::from(full_prompt)),
                "{} inputTokens must report the total prompt count",
                representation
            );
            prop_assert_eq!(
                value["usage"]["cacheReadInputTokens"].as_u64(),
                Some(u64::from(cache_read)),
                "{} cacheReadInputTokens must report the cached count",
                representation
            );
        }

        let tolerance = 1e-9 * expected_cost.abs().max(1.0);
        prop_assert!(
            (inclusive_cost - expected_cost).abs() <= tolerance,
            "inclusive {inclusive_cost} vs expected {expected_cost}"
        );
        prop_assert!(
            (exclusive_cost - expected_cost).abs() <= tolerance,
            "exclusive {exclusive_cost} vs expected {expected_cost}"
        );
        prop_assert!(
            (inclusive_cost - exclusive_cost).abs() <= tolerance,
            "inclusive {inclusive_cost} vs exclusive {exclusive_cost}"
        );
    }
}
