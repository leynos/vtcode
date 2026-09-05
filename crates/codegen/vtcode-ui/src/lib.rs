#![allow(
    missing_docs,
    clippy::expect_used,
    dead_code,
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
#![expect(
    unused_results,
    clippy::let_underscore_must_use,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "UI rendering uses validated display offsets, bounded terminal dimensions, and best-effort terminal cleanup."
)]
//! Unified UI crate for VT Code: design system, theme registry, and TUI framework.
//!
//! # Module layout
//!
//! - [`design`] — Colour conversion, style bridging, layout, diff, panel primitives
//! - [`theme`] — Theme registry, runtime state, syntax theme resolution
//! - [`tui`]   — Full TUI framework (session, widgets, runner, markdown, etc.)
//!
//! Items from `design` and `theme` are also re-exported at the crate root for
//! backward-compatibility with callers that previously imported from the
//! standalone `vtcode-design` / `vtcode-theme` crates (now consolidated into `vtcode-ui`).

pub mod design;
pub mod theme;
pub mod tui;
pub mod vim;

// Backward-compat re-exports so `vtcode_ui::ThemeStyles`, `vtcode_ui::colour::*`,
// etc. continue to work without path-qualified imports.
pub use design::*;
pub use theme::*;
