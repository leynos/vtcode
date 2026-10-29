//! Core's configuration-constant façade, including crate-private runtime limits.

pub use vtcode_config::constants::*;

/// Shared token ceiling for all leading summaries retained in bounded history.
/// This fixed allowance preserves room for complete user turns and recent work.
pub(crate) const LEADING_SUMMARY_ALLOWANCE_TOKENS: usize = 4_096;
