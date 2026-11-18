//! Catalogue lookup tests for model resolution.

use super::*;

/// The catalogue lookup must agree with resolution's static metadata.
///
/// Availability is the only thing resolution adds, and attribution that
/// uses this entry must therefore not depend on credential state.
#[test]
fn catalogue_entry_matches_resolved_static_metadata() {
    // Resolution trims the identifier before routing, so the padded spelling
    // must reach the same catalogue entry rather than lose its metadata.
    for model in ["claude-fable-5-1", " claude-fable-5-1 "] {
        let resolved = ModelResolver::resolve(Some("anthropic"), model, &[], None).expect("model");
        let entry = ModelResolver::catalogue_entry("anthropic", model).expect("catalogue entry");

        assert_eq!(Some(entry.context_window), resolved.context_window());
        assert_eq!(Some(entry.pricing), resolved.pricing());
        assert_eq!(entry.display_name, resolved.display_name().as_ref());
    }
}

/// A catalogue entry that declares no price resolves to no pricing.
///
/// Pricing is optional in the catalogue, so an undeclared price must stay
/// missing through resolution rather than appear as a free model.
#[test]
fn catalogue_entry_without_declared_pricing_resolves_to_no_pricing() {
    let resolved = ModelResolver::resolve(Some("anthropic"), "claude-fable-5", &[], None).expect("model");
    let entry = ModelResolver::catalogue_entry("anthropic", "claude-fable-5").expect("catalogue entry");
    let declared = entry.pricing.input.is_some()
        || entry.pricing.output.is_some()
        || entry.pricing.cache_read.is_some()
        || entry.pricing.cache_write.is_some();

    assert!(!declared, "the fixture model must declare no price");
    assert_eq!(resolved.pricing(), None);
}

/// A provider or model the catalogue does not carry yields no entry.
#[test]
fn catalogue_entry_reports_nothing_for_an_unknown_model() {
    assert!(ModelResolver::catalogue_entry("anthropic", "not-a-shipped-model").is_none());
    assert!(ModelResolver::catalogue_entry("not-a-provider", "claude-fable-5").is_none());
}
