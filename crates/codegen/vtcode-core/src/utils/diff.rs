//! Diff utilities for generating structured and formatted diffs.
//!
//! Delegates to `vtcode_ui::design::diff` for the canonical implementations.

pub use vtcode_commons::diff::*;

/// Format a unified diff without ANSI colour codes.
pub fn format_unified_diff(old: &str, new: &str, options: DiffOptions<'_>) -> String {
    vtcode_ui::design::diff::format_unified_diff(old, new, options)
}

/// Compute a structured diff bundle using the default theme-aware formatter.
pub fn compute_diff_with_theme(old: &str, new: &str, options: DiffOptions<'_>) -> DiffBundle {
    vtcode_ui::design::diff::compute_diff_with_theme(old, new, options)
}

/// Format diff hunks with standard ANSI colours for terminal display.
pub fn format_coloured_diff(hunks: &[DiffHunk], options: &DiffOptions<'_>) -> String {
    vtcode_ui::design::diff::format_coloured_diff(hunks, options)
}
