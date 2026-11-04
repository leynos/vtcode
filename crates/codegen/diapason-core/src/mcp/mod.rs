//! MCP client management built on top of the Codex MCP building blocks.
//!
//! Re-exported from `diapason-mcp` for backward compatibility.
//! The `cli` module remains local as it depends on `crate::cli::input_hardening`.

pub mod cli;
pub mod plugin_providers;

// Re-export everything from diapason-mcp
pub use diapason_mcp::*;
