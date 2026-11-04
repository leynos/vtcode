//! Shared UI protocol types used across Diapason crates.
//!
//! These types form the data model shared between `diapason-core` (the agent
//! library) and `diapason-ui` (the terminal surface). Extracting them here
//! lets headless builds compile without duplicating every enum and struct.
//!
//! The channel protocol types (`InlineCommand`, `InlineHandle`,
//! `InlineSession`) remain in the crate that owns them because the app-layer
//! and core-layer protocols diverge.

mod activity;
mod markdown;
mod selection;
mod style;
mod tool_summary;
mod types;

pub use activity::*;
pub use markdown::*;
pub use selection::*;
pub use style::*;
pub use tool_summary::*;
pub use types::*;
