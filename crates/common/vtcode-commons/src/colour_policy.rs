#![expect(
    unused_results,
    reason = "Forcing the one-time environment initialization is intentionally used only for its side effect."
)]

//! Runtime color output policy helpers.
//!
//! This module centralizes color enable/disable decisions for CLI and
//! transcript-style output paths. By default it follows the NO_COLOR
//! environment variable with strict "present and non-empty" semantics.

use once_cell::sync::Lazy;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Source that determined the active runtime color policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourOutputPolicySource {
    /// Default runtime behavior (auto detect + env hints).
    DefaultAuto,
    /// Disabled due to NO_COLOR environment variable.
    NoColourEnv,
    /// Disabled due to explicit `--no-colour` (or its `--no-color` alias).
    CliNoColour,
    /// Disabled due to explicit `--color never`.
    CliColourNever,
    /// Enabled due to explicit `--color always`.
    CliColourAlways,
    /// Enabled or disabled by explicit config override.
    ConfigOverride,
}

/// Runtime color output policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColourOutputPolicy {
    pub enabled: bool,
    pub source: ColourOutputPolicySource,
}

const SOURCE_DEFAULT_AUTO: u8 = 0;
const SOURCE_NO_COLOUR_ENV: u8 = 1;
const SOURCE_CLI_NO_COLOUR: u8 = 2;
const SOURCE_CLI_COLOUR_NEVER: u8 = 3;
const SOURCE_CLI_COLOUR_ALWAYS: u8 = 4;
const SOURCE_CONFIG_OVERRIDE: u8 = 5;

static POLICY_ENABLED: AtomicBool = AtomicBool::new(true);
static POLICY_SOURCE: AtomicU8 = AtomicU8::new(SOURCE_DEFAULT_AUTO);

static INIT_FROM_ENV: Lazy<()> = Lazy::new(|| {
    let default_policy = detect_policy_from_env();
    set_colour_output_policy(default_policy);
});

fn detect_policy_from_env() -> ColourOutputPolicy {
    if no_colour_env_active() {
        ColourOutputPolicy {
            enabled: false,
            source: ColourOutputPolicySource::NoColourEnv,
        }
    } else {
        ColourOutputPolicy {
            enabled: true,
            source: ColourOutputPolicySource::DefaultAuto,
        }
    }
}

fn encode_source(source: ColourOutputPolicySource) -> u8 {
    match source {
        ColourOutputPolicySource::DefaultAuto => SOURCE_DEFAULT_AUTO,
        ColourOutputPolicySource::NoColourEnv => SOURCE_NO_COLOUR_ENV,
        ColourOutputPolicySource::CliNoColour => SOURCE_CLI_NO_COLOUR,
        ColourOutputPolicySource::CliColourNever => SOURCE_CLI_COLOUR_NEVER,
        ColourOutputPolicySource::CliColourAlways => SOURCE_CLI_COLOUR_ALWAYS,
        ColourOutputPolicySource::ConfigOverride => SOURCE_CONFIG_OVERRIDE,
    }
}

fn decode_source(value: u8) -> ColourOutputPolicySource {
    match value {
        SOURCE_NO_COLOUR_ENV => ColourOutputPolicySource::NoColourEnv,
        SOURCE_CLI_NO_COLOUR => ColourOutputPolicySource::CliNoColour,
        SOURCE_CLI_COLOUR_NEVER => ColourOutputPolicySource::CliColourNever,
        SOURCE_CLI_COLOUR_ALWAYS => ColourOutputPolicySource::CliColourAlways,
        SOURCE_CONFIG_OVERRIDE => ColourOutputPolicySource::ConfigOverride,
        _ => ColourOutputPolicySource::DefaultAuto,
    }
}

fn no_colour_env_active_from(value: Option<OsString>) -> bool {
    value.map(|v| !v.is_empty()).unwrap_or(false)
}

/// Returns true when NO_COLOR is present and non-empty.
#[must_use]
pub fn no_colour_env_active() -> bool {
    no_colour_env_active_from(std::env::var_os("NO_COLOR"))
}

/// Read the current runtime color policy.
pub fn current_colour_output_policy() -> ColourOutputPolicy {
    Lazy::force(&INIT_FROM_ENV);
    ColourOutputPolicy {
        enabled: POLICY_ENABLED.load(Ordering::Relaxed),
        source: decode_source(POLICY_SOURCE.load(Ordering::Relaxed)),
    }
}

/// Replace the current runtime color policy.
pub fn set_colour_output_policy(policy: ColourOutputPolicy) {
    POLICY_ENABLED.store(policy.enabled, Ordering::Relaxed);
    POLICY_SOURCE.store(encode_source(policy.source), Ordering::Relaxed);
}

/// Reset runtime color policy from environment defaults.
pub fn reset_colour_output_policy_from_env() {
    set_colour_output_policy(detect_policy_from_env());
}

/// Returns true when runtime color output is enabled.
#[must_use]
pub fn colour_output_enabled() -> bool {
    current_colour_output_policy().enabled
}

#[cfg(test)]
mod tests {
    use super::no_colour_env_active_from;
    use std::ffi::OsString;

    #[test]
    fn no_colour_requires_non_empty_value() {
        assert!(!no_colour_env_active_from(None));
        assert!(!no_colour_env_active_from(Some(OsString::from(""))));
        assert!(no_colour_env_active_from(Some(OsString::from("1"))));
    }
}
