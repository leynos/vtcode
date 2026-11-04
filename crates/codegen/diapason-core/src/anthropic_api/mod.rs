//! Anthropic API compatibility layer for Diapason
//!
//! Provides compatibility with the Anthropic Messages API to help connect existing
//! applications to Diapason, including tools like Claude Code.

#[cfg(feature = "anthropic-api")]
pub use crate::llm::providers::anthropic::api as server;
